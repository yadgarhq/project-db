"""The four server limits (ADR-1006) are chart values, rendered and range-checked here.

`yadgar-lifecycle`'s `ServerLimits` has NO default (ADR-0569, ADR-0705): the binary
refuses to boot when one of `LISTEN_MAX_CONCURRENT_STREAMS`,
`LISTEN_KEEPALIVE_INTERVAL_SECS`, `LISTEN_KEEPALIVE_TIMEOUT_SECS`
or `LISTEN_MAX_HEADER_LIST_BYTES` is absent, not a
whole number or out of range. This chart supplies the defaults (`server.limits` in
`values.yaml`), renders them unconditionally, and refuses at RENDER what the binary
would refuse at boot. `tests/chart_server_limits.rs` pins the same defaults and ranges
against the library's own range constants; this file holds the render half.

Same discipline as `test_pool_sizing_schema.py`, which this file mirrors.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

CHART = Path(__file__).resolve().parents[2] / "chart"
CI_VALUES = CHART / "ci" / "values.yaml"

# (chart leaf, env var, ADR-1006 default, minimum, maximum). The ranges are
# `ServerLimits::*` in yadgar-lifecycle; `tests/chart_server_limits.rs` asserts them.
LIMITS: tuple[tuple[str, str, int, int, int], ...] = (
    ("maxConcurrentStreams", "LISTEN_MAX_CONCURRENT_STREAMS", 256, 1, 10_000),
    ("keepaliveIntervalSeconds", "LISTEN_KEEPALIVE_INTERVAL_SECS", 30, 1, 3_600),
    ("keepaliveTimeoutSeconds", "LISTEN_KEEPALIVE_TIMEOUT_SECS", 10, 1, 600),
    ("maxHeaderListBytes", "LISTEN_MAX_HEADER_LIST_BYTES", 16_384, 1_024, 1_048_576),
)


def render(*arguments: str) -> subprocess.CompletedProcess[str]:
    binary = shutil.which("helm")
    assert binary, (
        "helm is not on PATH. This suite renders the chart, and so does the "
        "`helm lint and render` pre-commit hook — install helm rather than skip."
    )
    return subprocess.run(
        [binary, "template", "limits", str(CHART), "--values", str(CI_VALUES), *arguments],
        capture_output=True,
        text=True,
    )


def rendered_value(manifest: str, variable: str) -> str:
    lines = manifest.splitlines()
    for i, line in enumerate(lines):
        if line.strip() == f"- name: {variable}":
            return lines[i + 1].strip()
    raise AssertionError(f"{variable} is not rendered at all")


def test_every_default_renders_as_its_variable() -> None:
    result = render()
    assert result.returncode == 0, result.stderr
    for _, variable, default, _, _ in LIMITS:
        assert rendered_value(result.stdout, variable) == f'value: "{default}"'


def test_a_value_at_each_end_of_its_range_renders() -> None:
    for leaf, variable, _, minimum, maximum in LIMITS:
        # The keepalive timeout must stay below the interval whatever else moves,
        # so an interval of 1 is unreachable (the timeout minimum is 1): its low
        # end is 2, with the timeout at 1.
        low = 2 if leaf == "keepaliveIntervalSeconds" else minimum
        extra: tuple[str, ...] = ()
        if leaf == "keepaliveIntervalSeconds":
            extra = ("--set", "server.limits.keepaliveTimeoutSeconds=1")
        if leaf == "keepaliveTimeoutSeconds":
            extra = ("--set", "server.limits.keepaliveIntervalSeconds=3600")
        for value in (low, maximum):
            result = render("--set", f"server.limits.{leaf}={value}", *extra)
            assert result.returncode == 0, f"{leaf}={value}: {result.stderr}"
            assert rendered_value(result.stdout, variable) == f'value: "{value}"'


def test_a_value_outside_its_range_is_refused_naming_the_key() -> None:
    for leaf, _, _, minimum, maximum in LIMITS:
        for value in (minimum - 1, maximum + 1):
            result = render("--set", f"server.limits.{leaf}={value}")
            assert result.returncode != 0, f"{leaf}={value} rendered"
            assert leaf in result.stderr, f"{leaf}={value}: {result.stderr}"


def test_an_explicit_null_is_refused_naming_the_key() -> None:
    for leaf, _, _, _, _ in LIMITS:
        result = render("--set", f"server.limits.{leaf}=null")
        assert result.returncode != 0, f"{leaf}: a nulled limit rendered"
        assert leaf in result.stderr, f"{leaf}: {result.stderr}"


def test_a_string_shaped_value_is_refused_naming_the_key() -> None:
    """`type: integer` is load-bearing: `--set-string` is what a quoted number produces."""
    for leaf, _, default, _, _ in LIMITS:
        result = render("--set-string", f"server.limits.{leaf}={default}")
        assert result.returncode != 0, f"{leaf}: a string-shaped limit rendered"
        assert leaf in result.stderr, f"{leaf}: {result.stderr}"


def test_a_nulled_server_block_is_refused() -> None:
    for shape in ("server=null", "server.limits=null", "server=x", "server.limits=x"):
        result = render("--set", shape)
        assert result.returncode != 0, f"{shape}: rendered"
        assert "server" in result.stderr, f"{shape}: {result.stderr}"


def test_a_keepalive_timeout_not_below_the_interval_is_refused() -> None:
    """The one cross-key rule. The schema cannot express it; a render check does."""
    for timeout in (30, 31):
        result = render("--set", f"server.limits.keepaliveTimeoutSeconds={timeout}")
        assert result.returncode != 0, f"timeout {timeout} against interval 30 rendered"
        assert "keepaliveTimeoutSeconds" in result.stderr, result.stderr
        assert "keepaliveIntervalSeconds" in result.stderr, result.stderr
    ok = render("--set", "server.limits.keepaliveTimeoutSeconds=29")
    assert ok.returncode == 0, ok.stderr


def test_a_large_value_renders_as_a_whole_number_not_a_float(tmp_path: Path) -> None:
    """helm decodes a YAML integer in a values FILE as a float64, and `quote` of
    1048576 prints `1.048576e+06`, which the binary refuses as not a whole number.
    (`--set` yields an int64 and would not show it.) The template converts with
    `int64` before `quote`; this pins the library's maximum."""
    values = tmp_path / "large.yaml"
    values.write_text(
        "server:\n  limits:\n    maxHeaderListBytes: 1048576\n    maxConcurrentStreams: 10000\n"
    )
    result = render("--values", str(values))
    assert result.returncode == 0, result.stderr
    assert (
        rendered_value(result.stdout, "LISTEN_MAX_HEADER_LIST_BYTES") == 'value: "1048576"'
    )
    assert rendered_value(result.stdout, "LISTEN_MAX_CONCURRENT_STREAMS") == 'value: "10000"'
