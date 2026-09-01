from __future__ import annotations
import importlib.util
from pathlib import Path
_path = Path(__file__).with_name("build-native-s3-plan.py")
_spec = importlib.util.spec_from_file_location("_native_s3_planner_impl", _path)
if _spec is None or _spec.loader is None:
    raise ImportError(f"cannot load {_path}")
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)
FamilyResolver = _mod.FamilyResolver
normalize_address = _mod.normalize_address
load_code_cache = _mod.load_code_cache
runtime_code_family = _mod.runtime_code_family
block_conflicts_by_owner = _mod.block_conflicts_by_owner
block_balanced_conflict_metrics = _mod.block_balanced_conflict_metrics
translate_call_tree = _mod.translate_call_tree
SEMANTIC_TRANSLATION_STATUSES = _mod.SEMANTIC_TRANSLATION_STATUSES
SYSTEM_TRANSLATION_STATUS = _mod.SYSTEM_TRANSLATION_STATUS
implementation_readiness = _mod.implementation_readiness
render_coverage_text = _mod.render_coverage_text
validate_frozen_map = _mod.validate_frozen_map
load_code_cache = _mod.load_code_cache

semantic_effect_for_entrypoint = _mod.semantic_effect_for_entrypoint
SEMANTIC_STATE_READ = _mod.SEMANTIC_STATE_READ
SEMANTIC_STATE_WRITE = _mod.SEMANTIC_STATE_WRITE
SEMANTIC_READ_WRITE = _mod.SEMANTIC_READ_WRITE
SEMANTIC_PURE = _mod.SEMANTIC_PURE
SEMANTIC_OPAQUE = _mod.SEMANTIC_OPAQUE
