"""Import shim for the funasr snap (feature 009).

funasr-onnx imports jieba at module level (utils/utils.py), but jieba is only
used by the CT-Transformer punctuation path (code-mix word splitting), which
is out of scope for this snap (FR-008: punctuation: false). The ~42 MB jieba
dict is pruned and replaced with this shim.

Note: SenseVoice can punctuate natively via textnorm="withitn" — no jieba
needed — so this shim does not foreclose a future punctuation feature.
"""


def __getattr__(name):
    raise RuntimeError(
        f"jieba is not packaged in this snap (punctuation path out of scope); "
        f"attempted to access jieba.{name}"
    )
