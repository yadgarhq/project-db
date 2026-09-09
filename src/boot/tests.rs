use super::*;
use yadgar_store::pool::MySqlSslMode;

/// An environment stating only what a test cares about.
fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
}

/// EVERY KNOB `pool_config` REQUIRES, WITH THE VALUE THE CHART WOULD RENDER.
///
/// There is no such thing as a partial environment for this function any
/// more, which is the whole of ADR-0569 restated as a fixture: a test that
/// used to pass `&[]` was asserting the defaults, and the defaults are gone.
///
/// THE VALUES ARE SENTINELS, not plausible ones. Nothing in this module, in
/// `store`, or in sqlx would arrive at `engine.example.invalid`, port 13306
/// or `verify-identity` on its own — so a field carrying one of these got it
/// from the lookup rather than from a fallback somebody left behind. The
/// former defaults (`127.0.0.1`, `3306`, `project`, `8`, `2`, `151`,
/// `required`) are deliberately absent from this list: a fixture repeating
/// them could not tell a read from a leftover default.
const RENDERED: [(&str, &str); 8] = [
    ("DB_HOST", "engine.example.invalid"),
    ("DB_PORT", "13306"),
    ("DB_NAME", "okapi_3f19_fixture"),
    ("DB_USER", "okapi_3f19_user"),
    ("DB_MAX_CONNECTIONS", "4"),
    ("REPLICAS", "3"),
    ("DB_ENGINE_MAX_CONNECTIONS", "137"),
    ("DB_SSL_MODE", "verify-identity"),
];

/// The rendered environment, with `overrides` winning over it.
fn env_with<'a>(overrides: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        overrides
            .iter()
            .chain(RENDERED.iter())
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
}

/// The rendered environment with ONE knob deleted — an unrendered variable.
fn env_without(omitted: &str) -> impl Fn(&str) -> Option<String> + '_ {
    move |key| {
        if key == omitted {
            return None;
        }
        RENDERED
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
}

/// `verify_identity` throughout, and never `required`.
///
/// `required` is what this module used to default to, so an implementation
/// that ignored the configuration entirely would pass a test fixed on it.
/// `verify_identity` is a mode neither this code nor sqlx would ever arrive
/// at on its own.
fn config_with(mode: MySqlSslMode) -> PoolConfig {
    PoolConfig {
        host: "engine.example.invalid".to_string(),
        port: 13306,
        database: "okapi_3f19_fixture".to_string(),
        username: "okapi_3f19_user".to_string(),
        max_connections: 4,
        replicas: 2,
        engine_max_connections: 151,
        ssl_mode: mode,
        ssl_ca: None,
    }
}

#[test]
fn the_probe_connects_with_the_configured_mode_not_a_mode_of_its_own() {
    let config = config_with(MySqlSslMode::VerifyIdentity);
    let options =
        probe_connect_options(&config, &Secret::new("fixture".to_string())).expect("options");

    // The defect this seam exists to prevent is a probe that describes its
    // own connection. Had it done so — hardcoding Required, or falling back
    // to sqlx's Preferred — this is the assertion that would not hold.
    assert!(
        matches!(options.get_ssl_mode(), MySqlSslMode::VerifyIdentity),
        "the probe did not take the configured ssl-mode"
    );

    // The rest of the connection comes from the same place, so a probe
    // pointed at a different host or database would be caught here too.
    // NO DSN in any message: an assert that interpolates one puts a username
    // into CI output.
    assert_eq!(options.get_host(), "engine.example.invalid");
    assert_eq!(options.get_port(), 13306);
    assert_eq!(options.get_username(), "okapi_3f19_user");
    assert_eq!(options.get_database(), Some("okapi_3f19_fixture"));
}

#[test]
fn certificate_verification_is_reachable_from_the_environment() {
    // The hyphen spelling is the one a chart writes; sqlx writes the
    // underscore.
    let config = pool_config(env_with(&[("DB_SSL_MODE", "verify-identity")])).expect("config");
    assert!(matches!(config.ssl_mode, MySqlSslMode::VerifyIdentity));
}

#[test]
fn verify_ca_is_refused_when_the_connection_options_are_built() {
    // `verify_ca` NAMES A CHECK NOTHING PERFORMS, which is why `store`
    // refuses it outright rather than documenting it. Measured in sqlx 0.9:
    // `sqlx-core`'s `net/tls/tls_rustls.rs` seeds the trust store with the
    // public web roots BEFORE appending the configured `ssl_ca`, so naming
    // an authority WIDENS trust and never restricts it; `sqlx-mysql`'s
    // `connection/tls.rs` then routes every mode but `VerifyIdentity`
    // through `NoHostnameTlsVerifier`, which maps a name mismatch to a
    // verified assertion.
    //
    // THE PROBE IS THE CALLER THIS PINS, and that is the point. The probe
    // opens its own connection before the pool exists, so a refusal that
    // lived only in `connect` would let the probe connect first — under the
    // very verifier being refused.
    let config = config_with(MySqlSslMode::VerifyCa);
    let err = probe_connect_options(&config, &Secret::new("fixture".to_string()))
        .expect_err("verify_ca must be refused before any connection is opened");

    assert!(
        matches!(err, BootError::Pool(PoolError::SslModeCannotVerify { .. })),
        "{err}"
    );

    // An operator reads this in a crash loop, so it has to name the mode
    // that WORKS. A refusal that only says no leaves them guessing between
    // three remaining values, two of which verify nothing either.
    let message = err.to_string();
    assert!(
        message.contains("verify_identity"),
        "the refusal must name the mode that does bind the engine's identity: {message}"
    );
}

/// A SENTINEL: nothing in this module or in `store` could produce this path,
/// so a test that sees it saw it travel from the environment.
const SENTINEL_CA: &str = "/etc/yadgar/okapi-3f19/engine-authority.pem";

#[test]
fn the_configured_certificate_authority_reaches_the_pool() {
    // A MODE IS NOT THE CAPABILITY. `verify_identity` checks a CHAIN, and
    // with no authority named sqlx falls back to the public web roots, which
    // sign no operator-issued engine certificate.
    let config = pool_config(env_with(&[("DB_SSL_CA_FILE", SENTINEL_CA)])).expect("config");

    assert_eq!(
        config.ssl_ca.as_deref(),
        Some(Path::new(SENTINEL_CA)),
        "the configured authority did not reach the pool configuration"
    );
}

#[test]
fn an_unset_or_empty_authority_is_no_authority_rather_than_an_empty_path() {
    // UNSET is the shipped deployment and must stay `None`: `Some` here
    // would name a file sqlx then fails to open, and a default CA path is a
    // policy this module has no business inventing.
    assert_eq!(pool_config(env_with(&[])).expect("config").ssl_ca, None);

    // EMPTY is the same statement written by a chart. Helm renders an unset
    // value as "", so a naive read turns "no authority" into `PathBuf::new()`
    // — a path sqlx opens and cannot, failing the boot of every deployment
    // that never asked for verification at all.
    for value in ["", " ", "\t", "\n"] {
        assert_eq!(
            pool_config(env_with(&[("DB_SSL_CA_FILE", value)]))
                .expect("config")
                .ssl_ca,
            None,
            "{value:?} must mean no authority, not an unopenable path"
        );
    }
}

#[test]
fn an_unrecognised_ssl_mode_refuses_the_boot_rather_than_falling_back() {
    // `yes` is not arbitrary: it is what a boolean-shaped answer looks like,
    // and failing open on a transport question is the class of bug rather
    // than one spelling of it.
    let err = pool_config(env_with(&[("DB_SSL_MODE", "yes")]))
        .expect_err("an unrecognised mode must refuse the boot");

    assert!(
        matches!(err, BootError::Pool(PoolError::UnknownSslMode { .. })),
        "{err}"
    );
}

/// THE PROPERTY THAT USED TO BE CALLED "the default encrypts", now that there
/// is no default to name.
///
/// What it guarded was never the value `required` — it was that an
/// unconfigured deployment could not silently arrive at `Preferred`, which
/// sqlx documents as falling back to an unencrypted connection. That
/// guarantee is STRUCTURAL now rather than a value: an environment naming no
/// mode does not reach `parse_ssl_mode` at all, it refuses the boot and names
/// DB_SSL_MODE. A default of `required` was the WEAKER form of the same
/// promise, because it also silently overrode an operator who meant to set a
/// mode and mistyped the variable's name.
#[test]
fn an_unset_ssl_mode_refuses_the_boot_rather_than_encrypting_by_default() {
    let err = pool_config(env_without(SSL_MODE_KEY))
        .expect_err("an unset ssl-mode must refuse the boot, not fall back to a value");

    let message = err.to_string();
    assert!(matches!(err, BootError::MissingKnob(_)), "{message}");
    assert!(message.contains(SSL_MODE_KEY), "{message}");

    // And the mode still travels when it IS named, so the refusal above is
    // not simply this function having stopped reading the variable.
    let config = pool_config(env_with(&[("DB_SSL_MODE", "required")])).expect("config");
    assert!(matches!(config.ssl_mode, MySqlSslMode::Required), "{err}");
}

/// THE ENGINE'S NAME AND USER TRAVEL FROM THE ENVIRONMENT, and no longer
/// from a constant in this file.
///
/// This test used to be `the_engine_defaults_name_this_module`, asserting
/// `project` out of an EMPTY environment — which is precisely the shape
/// ADR-0569 deletes. `chart/values.yaml` carries `database.name: project`
/// and `database.user: project` now, so a pod that connected to the wrong
/// database would be a chart edit rather than a copied constant. The
/// sentinels are what prove the read: nothing here could invent them.
#[test]
fn the_engines_name_and_user_travel_from_the_environment() {
    let config = pool_config(env_with(&[])).expect("config");
    assert_eq!(config.database, "okapi_3f19_fixture");
    assert_eq!(config.username, "okapi_3f19_user");
}

/// EVERY KNOB IS PROVED REQUIRED, ONE AT A TIME.
///
/// A single test on one variable would pass while seven others kept a
/// fallback nobody could see, so the loop is the assertion. The refusal has
/// to NAME the knob: three of these are numbers, and an empty or absent
/// number that reached `.parse()` would produce a `ParseIntError` saying
/// "cannot parse integer from empty string" and naming nothing at all — the
/// operator would learn only that some value was unreadable.
#[test]
fn every_rendered_knob_is_required_and_the_refusal_names_it() {
    for (key, _) in RENDERED {
        let err = pool_config(env_without(key)).err().unwrap_or_else(|| {
            panic!("{key} is absent from the environment and pool_config did not refuse")
        });

        let message = err.to_string();
        assert!(
            matches!(err, BootError::MissingKnob(_)),
            "{key} refused, but not as a missing knob: {message}"
        );
        assert!(
            message.contains(key),
            "the refusal must name the knob it wants; {key} produced: {message}"
        );
    }
}

/// **THE CASE THAT DISCRIMINATES.** Helm renders an unset value as `""`, so a
/// nulled chart value arrives as set-but-empty rather than as absent. An
/// implementation collapsing the two into one branch is a defect this estate
/// found three separate times in one week, so the two messages are asserted
/// to DIFFER rather than merely to exist.
#[test]
fn an_empty_knob_refuses_with_a_message_of_its_own() {
    for (key, _) in RENDERED {
        let empty = pool_config(env_with(&[(key, "")]))
            .expect_err("an empty knob must refuse the boot")
            .to_string();
        let absent = pool_config(env_without(key))
            .expect_err("an absent knob must refuse the boot")
            .to_string();

        assert!(empty.contains("set but EMPTY"), "{key}: {empty}");
        assert!(absent.contains("NOT SET"), "{key}: {absent}");
        assert_ne!(empty, absent, "{key}: empty and absent share one message");
    }
}

/// SENTINELS for the listener's configuration: nothing in this module could
/// produce either path, so a test that sees one saw it travel from the
/// lookup.
const SENTINEL_CERT: &str = "/etc/yadgar/okapi-3f19/serving.crt";
const SENTINEL_KEY: &str = "/etc/yadgar/okapi-3f19/serving.key";

/// THE DEFAULT for the listener: nothing configured means the cleartext
/// listener.
#[test]
fn nothing_configured_means_the_listener_serves_cleartext() {
    assert_eq!(ServeTls::from_lookup(LISTEN, env_of(&[])).unwrap(), None);
}

/// A certificate without the flag is the REVERTED state, not an error. The
/// flag is the lever; leaving the paths in place is how it gets pulled back.
#[test]
fn a_certificate_alone_does_not_enable_the_listeners_tls() {
    let vars = [
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    assert_eq!(ServeTls::from_lookup(LISTEN, env_of(&vars)).unwrap(), None);
}

/// Anything but "1" is off.
#[test]
fn only_exactly_one_enables_the_listeners_tls() {
    for value in ["0", "false", "no", "true", "yes", "", " "] {
        let vars = [
            ("LISTEN_TLS_ENABLED", value),
            ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
            ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
        ];
        assert_eq!(
            ServeTls::from_lookup(LISTEN, env_of(&vars)).unwrap(),
            None,
            "{value:?} must not enable TLS"
        );
    }
}

/// THE FAILURE THAT MUST NOT DEGRADE. Asking for TLS and naming neither file
/// is a deployment mistake, and the answer to it is an error rather than a
/// plaintext listener. The message names the half that is missing.
#[test]
fn asking_the_listener_for_tls_without_the_files_is_an_error() {
    let missing_cert = [
        ("LISTEN_TLS_ENABLED", "1"),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    assert!(
        matches!(
            ServeTls::from_lookup(LISTEN, env_of(&missing_cert)),
            Err(BootError::NoTlsCertFile("LISTEN"))
        ),
        "a missing certificate must be refused, not silently downgraded"
    );

    let missing_key = [
        ("LISTEN_TLS_ENABLED", "1"),
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
    ];
    assert!(
        matches!(
            ServeTls::from_lookup(LISTEN, env_of(&missing_key)),
            Err(BootError::NoTlsKeyFile("LISTEN"))
        ),
        "a missing private key must be refused, not silently downgraded"
    );
}

/// Both paths reach the settings, proved with names the module could not
/// have chosen for itself.
#[test]
fn the_certificate_and_the_key_both_arrive() {
    let vars = [
        ("LISTEN_TLS_ENABLED", "1"),
        ("LISTEN_TLS_CERT_FILE", SENTINEL_CERT),
        ("LISTEN_TLS_KEY_FILE", SENTINEL_KEY),
    ];
    let tls = ServeTls::from_lookup(LISTEN, env_of(&vars))
        .unwrap()
        .expect("a flag, a certificate and a key enable TLS");
    assert_eq!(tls.cert_file(), Path::new(SENTINEL_CERT));
    assert_eq!(tls.key_file(), Path::new(SENTINEL_KEY));
}

/// The two directions cannot configure each other. `DB_SSL_MODE` decides how
/// this module reaches its ENGINE and says nothing about what it serves, and
/// a bare `TLS_ENABLED` belongs to neither.
#[test]
fn the_engines_transport_does_not_configure_the_listener() {
    let vars = [
        ("DB_SSL_MODE", "verify-identity"),
        ("TLS_ENABLED", "1"),
        ("TLS_CERT_FILE", SENTINEL_CERT),
    ];
    assert_eq!(ServeTls::from_lookup(LISTEN, env_of(&vars)).unwrap(), None);
}

/// The crate returns one `io::Error` for both handlers, so what must not be
/// lost is the paragraph: an operator meeting this in a crash loop has to
/// read that BOTH signals are involved, that SIGTERM is the one Kubernetes
/// sends, and that there is no configuration value to go and correct.
///
/// **The `Err` path itself is not reachable from a test.** Producing it means
/// exhausting the process's signal-handler resources, which would break every
/// other test in the binary. So this asserts what an operator READS, not that
/// the failure occurs.
#[test]
fn a_handler_that_cannot_be_installed_names_both_signals_and_the_response() {
    let rendered = BootError::SignalHandler {
        source: std::io::Error::other("too many signal handlers"),
    }
    .to_string();

    for expected in [
        "SIGTERM",
        "SIGINT",
        "too many signal handlers",
        "no value to correct",
    ] {
        assert!(
            rendered.contains(expected),
            "the refusal no longer carries {expected:?}: {rendered}"
        );
    }
}
