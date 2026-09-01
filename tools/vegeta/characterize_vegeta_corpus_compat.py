from __future__ import annotations
import importlib.util
from pathlib import Path
_path = Path(__file__).with_name("characterize-vegeta-corpus.py")
_spec = importlib.util.spec_from_file_location("_vegeta_characterizer_impl", _path)
if _spec is None or _spec.loader is None:
    raise ImportError(f"cannot load {_path}")
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)
RpcClient = _mod.RpcClient
fetch_runtime_codes = _mod.fetch_runtime_codes
normalize_runtime_code = _mod.normalize_runtime_code
runtime_code_sha256 = _mod.runtime_code_sha256
normalize_address = _mod.normalize_address
normalize_block_call_trace_item = _mod.normalize_block_call_trace_item
count_call_frames = _mod.count_call_frames
iter_call_frames = _mod.iter_call_frames
