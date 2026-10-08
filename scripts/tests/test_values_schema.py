"""`chart/values.schema.json` closes this chart's key set (ledger 990, ADR-0847, ADR-0850).

WHAT THIS FILE ADDS. Before this PR the schema bounded exactly one knob
(`database.migrationLockTimeoutSeconds`, ledger 814) and left every other key
unvalidated: a typo anywhere else in `values.yaml` rendered silently. This file
closes every block the chart itself owns, declares the keys the templates read
that `values.yaml` does not (the EXTRAS below), and keeps four things open on
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
import re
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
#
# `tls.clientAuth`, `tls.clientCaSecret` and `tls.clientCaSecretKey` join this
# set for B-U5E (folded into C-DB1, ledger 1278): the schema declares them,
# `templates/render-checks.yaml` validates them when present, and the binary
# reads none of them yet — there is nothing for `values.yaml` to default,
# same shape as `image.digest` above (a template reads it; nothing ships it).
EXTRAS = frozenset(
    {
        "image.digest",
        "networkPolicy.scrapeFrom.namespace",
        "tls.clientAuth",
        "tls.clientCaSecret",
        "tls.clientCaSecretKey",
    }
)

# Leaves that carry real type information rather than the untyped `{}` every
# other leaf gets. `database.migrationLockTimeoutSeconds` shipped before
# ledger 990 and is kept unchanged by this PR. The four pool-sizing knobs
# (ledger C-DB2, ADR-0837, ADR-0849) join it for the same reason: each is
# `required` with a chart default `values.yaml` ships (so none belongs in
# `REQUIRED_NO_DEFAULT` below), and `required` alone passes a null value
# through (K-1), so the leaf needs its type too. `acquireTimeoutSeconds`
# alone also keeps a `maximum`, pinned against the dial's own request
# deadline by `tests/chart_request_deadline.rs`.
RETAINED_TYPED_LEAVES = frozenset(
    {
        "database.migrationLockTimeoutSeconds",
        "database.acquireTimeoutSeconds",
        "database.idleTimeoutSeconds",
        "database.maxLifetimeSeconds",
        "database.engineOperatorReserve",
    }
)

# `tls.enabled` (ledger 1278, ADR-0845, C-DB1): a leaf `values.yaml`
# DELIBERATELY ships no value for, because the knob's own absence is the
# property the schema exists to catch — a chart default here would be one
# more compiled-in default under the exact rule this unit enforces on the
# binary. Excluded from the values.yaml half of `expected_leaves` (there is
# no value to find there) and from the untyped-leaf check below (the leaf
# keeps `type: boolean`, same reason `RETAINED_TYPED_LEAVES` keeps its type:
# `required` alone passes a null value through, K-1). A SEPARATE set from
# `RETAINED_TYPED_LEAVES`, which is leaves shipped before ledger 990 and
# unrelated to it; this one is new, and its own check also asserts the leaf
# is in its parent block's `required` list, which `RETAINED_TYPED_LEAVES`'s
# does not (that property is `database`'s own pre-existing test, in
# `test_migration_lock_schema.py`).
REQUIRED_NO_DEFAULT = frozenset({"tls.enabled"})


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

    expected_leaves = values_leaves | OPEN | EXTRAS | REQUIRED_NO_DEFAULT
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

    for path in sorted(schema_leaves - RETAINED_TYPED_LEAVES - REQUIRED_NO_DEFAULT - OPEN):
        node = schema_node(schema, path)
        if node != {}:
            failures.append(f"leaf '{path}' should be untyped {{}}, got {node}")

    for path in sorted(RETAINED_TYPED_LEAVES):
        node = schema_node(schema, path)
        if node is None or node == {}:
            failures.append(f"retained typed leaf '{path}' lost its type")

    # ADR-0845's K-1: `required` ALONE passes a null value, so a leaf in
    # REQUIRED_NO_DEFAULT must carry both `type` AND appear in its parent
    # block's own `required` list — either half dropped lets the knob's
    # absence (or a null) reach the template unrefused by the schema.
    for path in sorted(REQUIRED_NO_DEFAULT):
        node = schema_node(schema, path)
        if node is None or node == {}:
            failures.append(f"required-no-default leaf '{path}' lost its type")
        parent_path, _, leaf = path.rpartition(".")
        parent = schema_node(schema, parent_path)
        if parent is None or leaf not in parent.get("required", []):
            failures.append(
                f"'{path}' is not in its parent block '{parent_path}'s `required` list"
            )

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


def test_mutation_dropping_tls_enabled_required_reddens() -> None:
    """ADR-0845's own mutation (ledger 1278): drop `tls.required: [enabled]`
    and the structural test must redden. `required` alone is what a chart
    default's absence depends on — see `test_tls_enabled_renders_unconditionally`
    below for the render-level half of the same property."""
    mutated = copy.deepcopy(SCHEMA)
    del mutated["properties"]["tls"]["required"]
    assert structural_failures(mutated, VALUES) != []


def test_mutation_dropping_tls_enabled_type_reddens() -> None:
    """`required` alone passes a null value (K-1); the leaf must keep its
    `type: boolean` too, independently of the `required` mutation above."""
    mutated = copy.deepcopy(SCHEMA)
    mutated["properties"]["tls"]["properties"]["enabled"] = {}
    assert structural_failures(mutated, VALUES) != []


def test_dropping_tls_required_on_disk_degrades_the_bare_lint_message(tmp_path) -> None:
    """THE RENDER-LEVEL PROOF the two structural mutations above exist for,
    and it is NOT "lint turns green" — measured, it does not. `templates/
    deployment.yaml` renders `ternary "1" "0" .Values.tls.enabled`
    UNCONDITIONALLY now (ADR-0845), and sprig's `ternary` raises a Go type
    error on anything but a bool, absent included — so the bare lint stays
    red with `required` gone too, on both helm 4.3.0 and 3.18.4 (measured).

    WHAT `required` (PLUS `type: boolean`) ACTUALLY BUYS is the MESSAGE.
    Correction #1 names it precisely: `templates/render-checks.yaml`'s own
    `fail` is invisible to `helm lint --strict` as an ERROR — the harness
    below shows it demoted to an `[INFO] Fail: …` line the lint output does
    not fail on — so with the schema intact, the operator reads the
    SCHEMA's own sentence naming the leaf; with `required` deleted on disk,
    the SAME bare lint still exits non-zero, but now on sprig's raw
    `wrong type for value`, which names no chart key at all. Dropping
    `required` is a real regression, measured as a WORSE message on a lint
    that was already red, not as a green one.

    ASSERTED ON THE WRAPPER SENTENCE ("values don't meet the specifications
    of the schema(s)"), NEVER ON HELM'S OWN PER-LEAF WORDING — measured to
    differ by version: helm 4.3.0 prints "missing property 'enabled'", helm
    3.18.4 prints "tls: enabled is required". Both carry the wrapper
    sentence this file's own module docstring already warns about (ADR-0650
    §"RED CASES ASSERT... NEVER HELM'S OWN SENTENCE"), which is the mistake
    an earlier revision of this test made and CI on 3.18.4 caught.
    """
    import shutil

    copy_dir = tmp_path / "chart"
    shutil.copytree(CHART, copy_dir)
    schema_copy = copy_dir / "values.schema.json"
    mutated = json.loads(schema_copy.read_text())
    del mutated["properties"]["tls"]["required"]
    schema_copy.write_text(json.dumps(mutated))

    from test_render_checks import helm

    before = helm("lint", "--strict", str(CHART))
    after = helm("lint", "--strict", str(copy_dir))

    assert before.returncode != 0, "the unmutated chart's bare lint must already refuse"
    assert after.returncode != 0, "dropping `required` must not turn the bare lint green"
    schema_wrapper = "values don't meet the specifications of the schema"
    assert schema_wrapper in before.stdout + before.stderr, (
        "the unmutated chart must report the schema's own validation wrapper: "
        f"{before.stdout}{before.stderr}"
    )
    assert schema_wrapper not in after.stdout + after.stderr, (
        "dropping `required` on disk should have lost the schema's validation "
        f"wrapper, leaving only the template's own crash: {after.stdout}{after.stderr}"
    )


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


# Adopted from task-db#83's test_values_schema.py (ledger 990 opus review):
# `assert_schema_refusal` above passed on a deeper wrong path, a substring key
# and even a non-schema error, because it only checked substring membership.
# `extract_refusal` parses EITHER measured helm shape into the same
# (path-segment-tuple, key) pair, and callers compare that pair exactly.
REFUSAL_SLASH_PATH = re.compile(
    r"at '([^']*)': additional propert(?:y|ies) '([^']+)'(?:, '[^']+')* (?:is |are )?not allowed"
)
REFUSAL_DOTTED_PATH = re.compile(
    r"^-\s+(\(root\)|[A-Za-z0-9_.\-]+):\s+Additional propert(?:y|ies)\s+(\S+)\s+(?:is|are)\s+not allowed",
    re.MULTILINE,
)


def extract_refusal(stderr: str) -> tuple[tuple[str, ...], str] | None:
    """The JSON path (as a tuple of segments) and the key name out of a schema
    refusal — on EITHER measured helm shape — never the sentence around them.
    """
    match = REFUSAL_SLASH_PATH.search(stderr)
    if match:
        raw_path, key = match.group(1), match.group(2)
        segments = tuple(raw_path.strip("/").split("/")) if raw_path.strip("/") else ()
        return segments, key

    match = REFUSAL_DOTTED_PATH.search(stderr)
    if match:
        raw_path, key = match.group(1), match.group(2)
        segments = () if raw_path == "(root)" else tuple(raw_path.split("."))
        return segments, key

    return None


def baseline_object_count() -> int:
    result = render(CHART)
    assert result.returncode == 0, result.stderr
    return len(objects(result.stdout))


BASELINE_OBJECTS = baseline_object_count()


def test_root_typo_is_refused_naming_the_key_at_the_root_path() -> None:
    result = render(CHART, "--set", "autoscalng.enabled=true")
    assert result.returncode != 0, "a root-level typo rendered"
    found = extract_refusal(result.stderr)
    assert found, result.stderr
    path, key = found
    assert path == (), (path, result.stderr)
    assert key == "autoscalng", (key, result.stderr)


def test_one_level_down_typo_is_refused_naming_the_key_and_the_parent_path() -> None:
    result = render(CHART, "--set", "autoscaling.enabeld=true")
    assert result.returncode != 0, "a typo one level down rendered"
    found = extract_refusal(result.stderr)
    assert found, result.stderr
    path, key = found
    assert path == ("autoscaling",), (path, result.stderr)
    assert key == "enabeld", (key, result.stderr)


def test_two_levels_down_typo_is_refused_naming_the_key_and_the_parent_path() -> None:
    result = render(CHART, "--set", "networkPolicy.scrapeFrom.namespac=x")
    assert result.returncode != 0, "a typo two levels down rendered"
    found = extract_refusal(result.stderr)
    assert found, result.stderr
    path, key = found
    assert path == ("networkPolicy", "scrapeFrom"), (path, result.stderr)
    assert key == "namespac", (key, result.stderr)


def test_twin_only_two_levels_down_typo_under_database_instance_storage() -> None:
    result = render(CHART, "--set", "database.instance.storage.siz=10Gi")
    assert result.returncode != 0, "a typo under database.instance.storage rendered"
    found = extract_refusal(result.stderr)
    assert found, result.stderr
    path, key = found
    assert path == ("database", "instance", "storage"), (path, result.stderr)
    assert key == "siz", (key, result.stderr)


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
    assert "`autoscaling.enabled` must be true or false" in result.stderr, result.stderr


def test_block_scalar_is_a_render_check_refusal_not_a_schema_refusal() -> None:
    result = render(CHART, "--set", "autoscaling=x")
    assert result.returncode != 0, "a scalar autoscaling block rendered"
    assert "schema" not in result.stderr.lower(), (
        "the schema refused this, not the render check:\n" + result.stderr
    )
    assert "`autoscaling` must be a map" in result.stderr, result.stderr


def test_deleted_leaf_is_a_render_check_refusal_not_a_schema_refusal() -> None:
    result = render(CHART, "--set", "autoscaling.enabled=null")
    assert result.returncode != 0, "a nulled leaf rendered"
    assert "schema" not in result.stderr.lower(), (
        "the schema refused this, not the render check:\n" + result.stderr
    )
    assert "`autoscaling.enabled` is absent" in result.stderr, result.stderr


def test_deleted_block_is_a_render_check_refusal_not_a_schema_refusal() -> None:
    result = render(CHART, "--set", "autoscaling=null")
    assert result.returncode != 0, "a nulled block rendered"
    assert "schema" not in result.stderr.lower(), (
        "the schema refused this, not the render check:\n" + result.stderr
    )
    assert "`autoscaling` is absent from the values" in result.stderr, result.stderr


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
    from test_render_checks import CI_VALUES, helm

    result = helm(
        "lint",
        "--strict",
        str(CHART),
        "--values",
        str(CI_VALUES),
        "--set",
        "autoscalng.enabled=true",
    )
    assert result.returncode != 0, "helm lint --strict passed a root-level typo"
    combined = result.stdout + result.stderr
    assert "[ERROR]" in combined and "autoscalng" in combined, combined
