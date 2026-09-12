"""Import shim for the funasr snap (feature 009).

funasr-onnx imports librosa at module level (sensevoice_bin.py etc.) but only
calls ``librosa.load()`` on file-path inputs. The myna adapter always decodes
in-memory numpy arrays, so librosa — and its numba/llvmlite/scipy/sklearn
chain, ~400 MB — is pruned from the snap and replaced with this shim.
"""


def load(*args, **kwargs):
    raise RuntimeError(
        "librosa is not packaged in this snap; pass numpy arrays, not file paths"
    )
