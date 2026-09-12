// Fused quantized SiLU custom ops for the Parakeet encoder.
//
// Replaces the per-island node chains the shipped export runs in fp32:
//
//   QSiLU:        DequantizeLinear -> Sigmoid -> Mul(x*sigma)            -> QuantizeLinear
//   QSiLUSmooth:  DequantizeLinear -> Sigmoid -> Mul(x*sigma) -> Mul(sm) -> QuantizeLinear
//
// with one kernel each: uint8 in, uint8 out, fp32 internal math, a single
// exit rounding (nearest-even, matching onnxruntime's QuantizeLinear) - the
// same rounding structure as the unfused fp32 chain, so this is a memory/
// kernel-count optimisation, not a numerics trade. An all-QLinear version of
// this idea was tried first and rejected: it rounded to 8 bits three extra
// times per island and measurably hurt WER; this op exists to get the
// fusion without those roundings.
//
// Portability (this needs to run on whatever CPU a user has, not just the
// reference machine): plain C++, no intrinsics, no ISA assumptions. exp() is
// a Cephes-style polynomial -
// branch-free, auto-vectorizable at -O3 on any target, and bit-identical
// across machines, which also makes output reproducible everywhere. QSiLU
// takes a 256-entry LUT path (the input is uint8: only 256 possible values)
// which is pure table lookup; QSiLUSmooth is a flat arithmetic loop
// parallelised over rows via onnxruntime's own thread pool.
//
// Build: see build.sh next to this file. ORT custom-op ABI: compiled against
// the 1.27 headers; register via SessionOptions.register_custom_ops_library.

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <vector>

// This library does not link against onnxruntime; the API surface arrives at
// registration time through RegisterCustomOps' OrtApiBase argument.
#define ORT_API_MANUAL_INIT
#include "onnxruntime_lite_custom_op.h"
#undef ORT_API_MANUAL_INIT

namespace {

// exp(x) for the sigmoid: max relative error ~2e-7 over the range that
// matters (|v| <= ~30; beyond that sigmoid saturates past fp32 resolution).
inline float round_half_even(float x) {
  // See quantize_u8: forces nearest-even rounding via the fp23 mantissa
  // boundary; valid for |x| < 2^22, branch-free, no libm call (floorf and
  // nearbyintf block vectorization at the x86-64 baseline).
  const float magic = 12582912.0f;  // 1.5 * 2^23
  return (x + magic) - magic;
}

inline float fast_expf(float x) {
  x = std::min(88.0f, std::max(-88.0f, x));
  float n = round_half_even(x * 1.44269504088896341f);
  float r = x - n * 0.693359375f;          // ln2 high part
  r -= n * -2.12194440e-4f;                // ln2 low part
  float p = 1.9875691500e-4f;
  p = p * r + 1.3981999507e-3f;
  p = p * r + 8.3334519073e-3f;
  p = p * r + 4.1665795894e-2f;
  p = p * r + 1.6666665459e-1f;
  p = p * r + 5.0000001201e-1f;
  p = p * r * r + r + 1.0f;
  union {
    float f;
    int32_t i;
  } s;
  s.i = (static_cast<int32_t>(n) + 127) << 23;  // 2^n
  return p * s.f;
}

inline float silu(float v) { return v / (1.0f + fast_expf(-v)); }

inline uint8_t quantize_u8(float t, float inv_y_scale, int32_t y_zp) {
  int32_t q = static_cast<int32_t>(round_half_even(t * inv_y_scale)) + y_zp;
  return static_cast<uint8_t>(std::min(255, std::max(0, q)));
}

void QSiLU(const Ort::Custom::Tensor<uint8_t>& X,
           const Ort::Custom::Tensor<float>& x_scale,
           const Ort::Custom::Tensor<uint8_t>& x_zp,
           const Ort::Custom::Tensor<float>& y_scale,
           const Ort::Custom::Tensor<uint8_t>& y_zp,
           Ort::Custom::Tensor<uint8_t>& Y) {
  const float xs = x_scale.Data()[0];
  const int32_t xz = x_zp.Data()[0];
  const float inv_ys = 1.0f / y_scale.Data()[0];
  const int32_t yz = y_zp.Data()[0];

  // uint8 input: the whole op is a 256-entry table.
  uint8_t lut[256];
  for (int v = 0; v < 256; ++v) {
    lut[v] = quantize_u8(silu(xs * static_cast<float>(v - xz)), inv_ys, yz);
  }

  const uint8_t* in = X.Data();
  const int64_t n = X.NumberOfElement();
  uint8_t* out = Y.Allocate(X.Shape());
  for (int64_t i = 0; i < n; ++i) out[i] = lut[in[i]];
}

struct SmoothWork {
  const uint8_t* in;
  uint8_t* out;
  const float* smooth;
  int64_t channels;
  float xs;
  int32_t xz;
  float inv_ys;
  int32_t yz;
};

// target_clones: the compiler emits baseline, AVX2 and AVX-512 versions of
// this one function and dispatches by CPUID at load - wider vectors where
// the machine has them, the portable baseline everywhere else, no
// hand-written intrinsics and no silent breakage on older CPUs. The hot
// loop is written to stay branch-free so all three clones vectorize.
__attribute__((target_clones("default", "avx2", "arch=x86-64-v4"))) void SmoothRow(void* usr,
                                                                                   size_t row) {
  const SmoothWork& w = *static_cast<const SmoothWork*>(usr);
  // Everything loop-invariant lives in locals with restrict pointers: the
  // uint8 output stores otherwise legally alias the loop bound and the
  // smooth vector (char* aliases anything), which blocks vectorization
  // outright ("number of iterations cannot be computed").
  const int64_t channels = w.channels;
  const uint8_t* __restrict in = w.in + row * channels;
  uint8_t* __restrict out = w.out + row * channels;
  const float* __restrict smooth = w.smooth;
  const float xs = w.xs, inv_ys = w.inv_ys;
  const float xzf = static_cast<float>(w.xz), yzf = static_cast<float>(w.yz);
  for (int64_t c = 0; c < channels; ++c) {
    float v = xs * (static_cast<float>(in[c]) - xzf);
    float t = silu(v) * smooth[c];
    float q = round_half_even(t * inv_ys) + yzf;
    q = std::min(255.0f, std::max(0.0f, q));
    out[c] = static_cast<uint8_t>(q);
  }
}

void QSiLUSmooth(OrtKernelContext* context,
                 const Ort::Custom::Tensor<uint8_t>& X,
                 const Ort::Custom::Tensor<float>& x_scale,
                 const Ort::Custom::Tensor<uint8_t>& x_zp,
                 const Ort::Custom::Tensor<float>& smooth,
                 const Ort::Custom::Tensor<float>& y_scale,
                 const Ort::Custom::Tensor<uint8_t>& y_zp,
                 Ort::Custom::Tensor<uint8_t>& Y) {
  const int64_t channels = smooth.NumberOfElement();
  const int64_t n = X.NumberOfElement();
  SmoothWork w{X.Data(),          Y.Allocate(X.Shape()), smooth.Data(), channels,
               x_scale.Data()[0], x_zp.Data()[0],        0.0f,          y_zp.Data()[0]};
  w.inv_ys = 1.0f / y_scale.Data()[0];

  const size_t rows = static_cast<size_t>(n / channels);
  Ort::KernelContext ctx(context);
  ctx.ParallelFor(SmoothRow, rows, /*num_batch=*/4, &w);
}

}  // namespace

static const char* kDomain = "myna";

extern "C" OrtStatus* ORT_API_CALL RegisterCustomOps(OrtSessionOptions* options,
                                                     const OrtApiBase* api_base) {
  Ort::InitApi(api_base->GetApi(ORT_API_VERSION));
  static auto op_silu =
      std::unique_ptr<OrtCustomOp>(Ort::Custom::CreateLiteCustomOp("QSiLU", "CPUExecutionProvider", QSiLU));
  static auto op_smooth = std::unique_ptr<OrtCustomOp>(
      Ort::Custom::CreateLiteCustomOp("QSiLUSmooth", "CPUExecutionProvider", QSiLUSmooth));
  static Ort::CustomOpDomain domain = [] {
    Ort::CustomOpDomain d{kDomain};
    d.Add(op_silu.get());
    d.Add(op_smooth.get());
    return d;
  }();
  return Ort::GetApi().AddCustomOpDomain(options, domain);
}
