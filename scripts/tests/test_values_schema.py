"""`chart/values.schema.json` closes this chart's key set (ledger 990, ADR-0847, ADR-0850).

WHAT THIS FILE ADDS. Before this PR the schema bounded exactly one knob
(`database.migrationLockTimeoutSeconds`, ledger 814) and left every other key
unvalidated: a typo anywhere else in `values.yaml` rendered silently. This file
closes every block the chart itself owns, declares the keys the templates read
that `values.yaml` does not (the EXTRAS below), and keeps three things open on
purpose (the OPEN paths below) because a template hands them straight to
`toYaml` or `with` and any shape is legal there.

TYPES, TOGGLES AND `required` ARE A RENDER CHECK'S JOB, NOT THIS SCHEMA'S
(ADR-0847). `templates/render-checks.yaml` already refuses a non-bool
`autoscaling.enabled`, a non-bool `database.create` and a non-map
`autoscaling`, each by name; this schema adds nothing on top of those checks
and the red cases below assert that a shape refusal still comes from the
render check, not from the schema, by checking the stderr never carries a
schema-shaped sentence. The two pre-existing typed leaves
(`database.migrationLockTimeoutSeconds`, and `database` itself via
`type: object` + `required`) are untouched; `test_migration_lock_schema.py`
is their gate and is not duplicated here.

RED CASES ASSERT THE KEY NAME AND THE JSON PATH ONLY, NEVER HELM'S OWN
SENTENCE. helm 3.20.2 and helm 4.3.0 print the same shape
(`at '<path>': additional properties '<key>' not allowed`) but 3.18.4 does
not, and the wording is helm's to change; the key and the path are this
schema's contract.

THE STRUCTURAL TESTS BELOW ARE PURE: they read `chart/values.yaml` and
`chart/values.schema.json` and compare shapes, with no helm invocation. They
re-derive the closure rule independently of `gen_schema.py` (which is a
drafting aid, not part of this chart), so a schema edited by hand that drifts
from the rule is caught here rather than only by a render.

Run: python3 -m pytest scripts/tests/ -q
"""

from __future__ import annotations

import copy
import json
from pathlib import Path
from typing import Any

import yaml

from test_render_checks import CHART, objects, render

SCHEMA_PATH = CHART / "values.schema.json"
VALUES_PATH = CHART / "values.yaml"

# Every map this chart hands straight to `toYaml`/`with` with no further
# validation (brief §3.4). `global` is the one key every module forwards for
# the parent but never declares in its own `values.yaml`.
OPEN = frozenset(
    {
        "global",
        "resources",
        "rollingUpdate",
        "database.instance.resources",
    }
)

# A template reads these; `values.yaml` does not declare them (brief §2 step 2,
# §3.6). Declared as leaves inside their (now closed) parent block.
EXTRAS = frozenset(
    {
        "image.digest",
        "networkPolicy.scrapeFrom.namespace",
    }
)

# Leaves that carry real type information (shipped before ledger 990, kept
# unchanged by this PR) rather than the untyped `{}` every other leaf gets.
RETAINED_TYPED_LEAVES = frozenset({"database.migrationLockTimeoutSeconds"})


def load_schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text())


def load_values() -> dict:
    return yaml.safe_load(VALUES_PATH.read_text()) or {}


SCHEMA = load_schema()
VALUES = load_values()


def schema_node(schema: dict, path: str) -> Any:
    """The schema node at a dotted path, or None if no `properties` chain reaches it."""
    if path == "":
        return schema
    node = schema
    for step in path.split("."):
        if not isinstance(node, dict) or step not in node.get("properties", {}):
            return None
        node = node["properties"][step]
    return node


def values_leaves_and_blocks(values: dict) -> tuple[set[str], set[str]]:
    """Every (leaf path, block path) in `values.yaml`, stopping at an OPEN path. PURE."""
    leaves: set[str] = set()
    blocks: set[str] = set()

    def walk(node: Any, prefix: str) -> None:
        if prefix in OPEN:
            return
        if isinstance(node, dict):
            blocks.add(prefix)
            for key, child in node.items():
                walk(child, f"{prefix}.{key}" if prefix else key)
        else:
            leaves.add(prefix)

    walk(values, "")
    return leaves, blocks


def schema_leaves_and_blocks(schema: dict) -> tuple[set[str], set[str]]:
    """Every (leaf path, block path) declared in the schema. PURE."""
    leaves: set[str] = set()
    blocks: set[str] = set()

    def walk(node: dict, prefix: str) -> None:
        if "properties" in node:
            blocks.add(prefix)
            for key, child in node["properties"].items():
                walk(child, f"{prefix}.{key}" if prefix else key)
        elif prefix:
            leaves.add(prefix)

    walk(schema, "")
    return leaves, blocks


def structural_failures(schema: dict, values: dict) -> list[str]:
    """Every way `schema` can disagree with `values` plus the shape rule above. PURE."""
    failures: list[str] = []

    values_leaves, values_blocks = values_leaves_and_blocks(values)
    schema_leaves, schema_blocks = schema_leaves_and_blocks(schema)

    expected_leaves = values_leaves | OPEN | EXTRAS
    if schema_leaves != expected_leaves:
        failures.append(
            "declared leaves disagree with values.yaml + OPEN + EXTRAS: "
            f"missing={sorted(expected_leaves - schema_leaves)} "
            f"extra={sorted(schema_leaves - expected_leaves)}"
        )

    expected_blocks = values_blocks | {""}
    if schema_blocks != expected_blocks:
        failures.append(
            "declared blocks disagree with values.yaml: "
            f"missing={sorted(expected_blocks - schema_blocks)} "
            f"extra={sorted(schema_blocks - expected_blocks)}"
        )

    for path in sorted(schema_blocks):
        node = schema_node(schema, path)
        if node is None or node.get("additionalProperties") is not False:
            failures.append(f"block '{path}' does not carry additionalProperties: false")

    for path in sorted(OPEN):
        node = schema_node(schema, path)
        if node != {}:
            failures.append(f"OPEN path '{path}' is not a bare {{}} in the schema: {node}")

    for path in sorted(schema_leaves - RETAINED_TYPED_LEAVES - OPEN):
        node = schema_node(schema, path)
        if node != {}:
            failures.append(f"leaf '{path}' should be untyped {{}}, got {node}")

    for path in sorted(RETAINED_TYPED_LEAVES):
        node = schema_node(schema, path)
        if node is None or node == {}:
            failures.append(f"retained typed leaf '{path}' lost its type")

    if schema.get("additionalProperties") is not False:
        failures.append("root additionalProperties is not false")

    return failures


def test_the_schema_matches_values_yaml_exactly() -> None:
    failures = structural_failures(SCHEMA, VALUES)
    assert failures == [], "\n".join(failures)


def test_mutation_deleting_root_additional_properties_reddens() -> None:
    mutated = copy.deepcopy(SCHEMA)
    del mutated["additionalProperties"]
    assert structural_failures(mutated, VALUES) != []


def test_mutation_deleting_global_reddens() -> None:
    mutated = copy.deepcopy(SCHEMA)
    del mutated["properties"]["global"]
    assert structural_failures(mutated, VALUES) != []


def test_mutation_deleting_an_extra_reddens() -> None:
    mutated = copy.deepcopy(SCHEMA)
    del mutated["properties"]["image"]["properties"]["digest"]
    assert structural_failures(mutated, VALUES) != []


def test_mutation_closing_an_open_map_reddens() -> None:
    mutated = copy.deepcopy(SCHEMA)
    mutated["properties"]["resources"] = {"properties": {}, "additionalProperties": False}
    assert structural_failures(mutated, VALUES) != []


# ---------------------------------------------------------------------------
# Render-level red/green cases (brief §5). Every assertion below reads the key
# name and the JSON path fragment only, never helm's own sentence — and reads
# the path in whichever of the two shapes the running helm prints. Measured:
# helm 3.18.4 prints a dotted parent path with no quoting around the key
# (`- autoscaling: Additional property enabeld is not allowed`, root case
# `- (root): Additional property autoscalng is not allowed`); helm 3.20.2 and
# 4.3.0 print a JSON-pointer path with the key quoted
# (`- at '/autoscaling': additional properties 'enabeld' not allowed`, root
# case `- at '': additional properties 'autoscalng' not allowed`). CI's
# `ci/precommit` job pins 3.18.4; this suite is also run locally against
# 3.20.2 and 4.3.0 (brief's measurement set), so both shapes must pass.


def assert_schema_refusal(stderr: str, key: str, path: tuple[str, ...] = ()) -> None:
    assert key in stderr, stderr
    if path:
        dotted = ".".join(path)
        pointer = "/" + "/".join(path)
        assert dotted in stderr or pointer in stderr, stderr
    else:
        assert "(root)" in stderr or "at ''" in stderr, stderr


def baseline_object_count() -> int:
    result = render(CHART)
    assert result.returncode == 0, result.stderr
    return len(objects(result.stdout))


BASELINE_OBJECTS = baseline_object_count()


def test_root_typo_is_refused_naming_the_key_at_the_root_path() -> None:
    result = render(CHART, "--set", "autoscalng.enabled=true")
    assert result.returncode != 0, "a root-level typo rendered"
    assert_schema_refusal(result.stderr, "autoscalng")


def test_one_level_down_typo_is_refused_naming_the_key_and_the_parent_path() -> None:
    result = render(CHART, "--set", "autoscaling.enabeld=true")
    assert result.returncode != 0, "a typo one level down rendered"
    assert_schema_refusal(result.stderr, "enabeld", ("autoscaling",))


def test_two_levels_down_typo_is_refused_naming_the_key_and_the_parent_path() -> None:
    result = render(CHART, "--set", "networkPolicy.scrapeFrom.namespac=x")
    assert result.returncode != 0, "a typo two levels down rendered"
    assert_schema_refusal(result.stderr, "namespac", ("networkPolicy", "scrapeFrom"))


def test_twin_only_two_levels_down_typo_under_database_instance_storage() -> None:
    result = render(CHART, "--set", "database.instance.storage.siz=10Gi")
    assert result.returncode != 0, "a typo under database.instance.storage rendered"
    assert_schema_refusal(result.stderr, "siz", ("database", "instance", "storage"))


def test_wrong_type_toggle_is_a_render_check_refusal_not_a_schema_refusal() -> None:
    result = render(
        CHART,
        "--api-versions",
        "keda.sh/v1alpha1",
        "--set-string",
        "autoscaling.enabled=false",
    )
    assert result.returncode != 0, "a string toggle rendered"
    assert "schema" not in result.stderr.lower(), (
        "the schema refused this, not the render check:\n" + result.stderr
    )


def test_block_scalar_is_a_render_check_refusal_not_a_schema_refusal() -> None:
    result = render(CHART, "--set", "autoscaling=x")
    assert result.returncode != 0, "a scalar autoscaling block rendered"
    assert "schema" not in result.stderr.lower(), (
        "the schema refused this, not the render check:\n" + result.stderr
    )


def test_deleted_leaf_is_a_render_check_refusal_not_a_schema_refusal() -> None:
    result = render(CHART, "--set", "autoscaling.enabled=null")
    assert result.returncode != 0, "a nulled leaf rendered"
    assert "schema" not in result.stderr.lower(), (
        "the schema refused this, not the render check:\n" + result.stderr
    )


def test_deleted_block_is_a_render_check_refusal_not_a_schema_refusal() -> None:
    result = render(CHART, "--set", "autoscaling=null")
    assert result.returncode != 0, "a nulled block rendered"
    assert "schema" not in result.stderr.lower(), (
        "the schema refused this, not the render check:\n" + result.stderr
    )


def test_open_map_resources_accepts_any_shape() -> None:
    result = render(CHART, "--set", "resources.foo.bar=1")
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == BASELINE_OBJECTS


def test_open_map_global_accepts_any_shape() -> None:
    result = render(CHART, "--set", "global.whatever=1")
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == BASELINE_OBJECTS


def test_open_map_rolling_update_accepts_any_shape() -> None:
    result = render(CHART, "--set", "rollingUpdate.partition=1")
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == BASELINE_OBJECTS


def test_open_map_database_instance_resources_accepts_any_shape() -> None:
    result = render(CHART, "--set", "database.instance.resources.foo=1")
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == BASELINE_OBJECTS


def test_extra_image_digest_renders() -> None:
    result = render(
        CHART,
        "--set",
        "image.digest=sha256:" + "a" * 64,
    )
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == BASELINE_OBJECTS


def test_untyped_leaf_accepts_a_string_override() -> None:
    result = render(CHART, "--set-string", "replicaCount=2")
    assert result.returncode == 0, result.stderr
    assert len(objects(result.stdout)) == BASELINE_OBJECTS


def test_lint_strict_refuses_the_same_root_typo_naming_the_key() -> None:
    from test_render_checks import helm

    result = helm(
        "lint",
        "--strict",
        str(CHART),
        "--set",
        "autoscalng.enabled=true",
    )
    assert result.returncode != 0, "helm lint --strict passed a root-level typo"
    combined = result.stdout + result.stderr
    assert "[ERROR]" in combined and "autoscalng" in combined, combined
