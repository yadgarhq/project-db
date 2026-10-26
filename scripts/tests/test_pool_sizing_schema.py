"""The four pool-sizing knobs' bounds live in ONE place: chart/values.schema.json.

Ledger C-DB2, ADR-0837, ADR-0849. `yadgar-store` v0.4.0 stopped defaulting
`acquire_timeout`, `idle_timeout`, `max_lifetime` and `operator_reserve`, and this
chart's `database.acquireTimeoutSeconds`, `database.idleTimeoutSeconds`,
`database.maxLifetimeSeconds` and `database.engineOperatorReserve` are now the
only source for all four — each rendered unconditionally and read by the binary
with no fallback. The binary refuses an unusable value at boot; this file holds
the chart's half: the schema refuses the same mistakes at RENDER, before
anything is deployed. Same discipline as `test_migration_lock_schema.py`, which
this file mirrors.

THE NULL CASE IS THE ONE THAT MATTERS, same reasoning as the migration lock's:
Helm deep-merges an adopter's values over `chart/values.yaml`, so leaving a key
out inherits the shipped default — only an explicit `null` removes it. Without
`required` in the schema that null rendered an EMPTY env var and the pod refused
at boot; with it, the render refuses and names the key. Measured on helm 3.18.4
(Argo's) and 4.3.0.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

CHART = Path(__file__).resolve().parents[2] / "chart"

# (chart key, env var, shipped value, upper bound or None)
KNOBS: tuple[tuple[str, str, str, int | None], ...] = (
    ("database.acquireTimeoutSeconds", "DB_ACQUIRE_TIMEOUT_SECONDS", "12", 13),
    ("database.idleTimeoutSeconds", "DB_IDLE_TIMEOUT_SECONDS", "600", None),
    ("database.maxLifetimeSeconds", "DB_MAX_LIFETIME_SECONDS", "1800", None),
    ("database.engineOperatorReserve", "DB_ENGINE_OPERATOR_RESERVE", "5", None),
)

# ADR-0845 (ledger 1278, C-DB1): `tls.enabled` lost its chart default and is
# now `required`, so a bare render of this chart refuses on that alone — with
# nothing about pool sizing in the diagnostic. Applied FIRST, same as
# `test_migration_lock_schema.py::render`, so an `arguments` override still wins.
CI_VALUES = CHART / "ci" / "values.yaml"


def render(*arguments: str) -> subprocess.CompletedProcess[str]:
    binary = shutil.which("helm")
    assert binary, (
        "helm is not on PATH. This suite renders the chart, and so does the "
        "`helm lint and render` pre-commit hook — install helm rather than skip."
    )
    return subprocess.run(
        [binary, "template", "lock", str(CHART), "--values", str(CI_VALUES), *arguments],
        capture_output=True,
        text=True,
    )


def rendered_value(manifest: str, variable: str) -> str:
    lines = manifest.splitlines()
    for i, line in enumerate(lines):
        if line.strip() == f"- name: {variable}":
            return lines[i + 1].strip()
    raise AssertionError(f"{variable} is not rendered at all")


def test_every_shipped_value_renders() -> None:
    result = render()
    assert result.returncode == 0, result.stderr
    for _, variable, shipped, _ in KNOBS:
        assert rendered_value(result.stdout, variable) == f'value: "{shipped}"'


def test_a_stated_value_inside_every_bound_renders() -> None:
    for knob, variable, _, maximum in KNOBS:
        value = maximum if maximum is not None else 2_000
        result = render("--set", f"{knob}={value}")
        assert result.returncode == 0, f"{knob}: {result.stderr}"
        assert rendered_value(result.stdout, variable) == f'value: "{value}"'


# A VALUES-FILE NUMBER IS A FLOAT64, not the int64 `--set` hands the template
# (every render above). `quote` prints a float64 of a million or more in
# exponent form: `maxLifetimeSeconds: 1000000` in a values file rendered
# "1e+06", which the binary's integer parse refuses at boot. Every typed knob's
# env line casts with `int64` first; the schema's `"type": "integer"` already
# refused a fraction or a string, so the cast cannot hide one. The parent chart
# hands this chart its values the same float64 way.
def test_a_large_value_from_a_values_file_renders_as_an_integer(tmp_path: Path) -> None:
    for knob, variable, _, maximum in KNOBS:
        value = maximum if maximum is not None else 1_000_000
        block, leaf = knob.split(".")
        values_file = tmp_path / "values.yaml"
        values_file.write_text(f"{block}:\n  {leaf}: {value}\n")
        result = render("--values", str(values_file))
        assert result.returncode == 0, f"{knob}: {result.stderr}"
        assert rendered_value(result.stdout, variable) == f'value: "{value}"', knob


def test_an_explicit_null_is_refused_at_render_naming_the_key() -> None:
    for knob, _, _, _ in KNOBS:
        leaf = knob.rsplit(".", 1)[-1]
        result = render("--set", f"{knob}=null")
        assert result.returncode != 0, f"{knob}: a nulled knob rendered"
        assert leaf in result.stderr, f"{knob}: {result.stderr}"


def test_zero_is_refused_at_render() -> None:
    for knob, _, _, _ in KNOBS:
        leaf = knob.rsplit(".", 1)[-1]
        result = render("--set", f"{knob}=0")
        assert result.returncode != 0, f"{knob}: a zero knob rendered"
        assert leaf in result.stderr, f"{knob}: {result.stderr}"


def test_above_the_bound_is_refused_at_render() -> None:
    for knob, _, _, maximum in KNOBS:
        if maximum is None:
            continue
        leaf = knob.rsplit(".", 1)[-1]
        result = render("--set", f"{knob}={maximum + 1}")
        assert result.returncode != 0, f"{knob}: a value above {maximum} rendered"
        assert leaf in result.stderr, f"{knob}: {result.stderr}"


def test_a_nulled_database_block_is_refused_at_render() -> None:
    result = render("--set", "database=null")
    assert result.returncode != 0, "a nulled database block rendered"


def test_a_string_shaped_value_is_refused_at_render() -> None:
    """`type: integer` is LOAD-BEARING, not decoration `minimum`/`maximum`
    would catch anyway. `--set` alone cannot produce this case — it infers a
    bare digit string as JSON's own integer — so this uses `--set-string`,
    which is what a values override that quotes a number (`"25"`) also
    produces. Dropping `type: integer` from the schema would still catch an
    out-of-bound number but would pass this value straight through to the
    binary as a string the chart's own `| quote` then doubles, which is a
    different render than every other case in this file exercises.
    """
    for knob, _, shipped, _ in KNOBS:
        leaf = knob.rsplit(".", 1)[-1]
        result = render("--set-string", f"{knob}={shipped}")
        assert result.returncode != 0, f"{knob}: a string-shaped value rendered"
        assert leaf in result.stderr, f"{knob}: {result.stderr}"


# THE DATABASE ACCOUNT'S OWN CAP (ADR-1006, yaadgaar #200). mariadb-operator
# writes `maxUserConnections: 20` on the User it creates from
# `MariaDB.spec.username` (measured live on kind-yadgar, 2026-10-24), and the
# MariaDB CRD has no knob for it. `yadgar-store` refuses at boot when
# maxConnections x REPLICAS exceeds that cap. So the shipped pool, at the most
# replicas this chart can run, must fit 20 — or a default install crash-loops
# once it takes the store release carrying that check. A chart-owned User CR
# with a computed cap is the follow-up (yaadgaar #411).
OPERATOR_WRITTEN_USER_CAP = 20


def test_the_shipped_pool_fits_the_operator_written_user_cap() -> None:
    import yaml

    values = yaml.safe_load((CHART / "values.yaml").read_text())
    pool = values["database"]["maxConnections"]
    replicas = max(values["autoscaling"]["maxReplicas"], values["replicaCount"])
    assert pool * replicas <= OPERATOR_WRITTEN_USER_CAP, (
        f"database.maxConnections {pool} x {replicas} replicas = {pool * replicas} "
        f"exceeds the operator-written MAX_USER_CONNECTIONS {OPERATOR_WRITTEN_USER_CAP}"
    )


def test_the_shipped_pool_is_three_per_replica() -> None:
    result = render()
    assert result.returncode == 0, result.stderr
    assert rendered_value(result.stdout, "DB_MAX_CONNECTIONS") == 'value: "3"'
