use std::f64::consts::{FRAC_PI_2, PI};
use volna::model::{Profile, Signal, Source};
use volna::sources::CompiledSource;

fn formula(expression: &str) -> Source {
    Source {
        frequency_hz: 1.0,
        fmax_hz: 2.0,
        aperture: Profile::Uniform,
        signal: Signal::Formula {
            expression: expression.into(),
        },
        ..Source::default()
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
        "{actual} != {expected}"
    );
}

#[test]
fn integer_literals_always_have_real_arithmetic() -> volna::Result<()> {
    for (text, expected) in [
        ("1/2", 0.5),
        ("(1/2)+(3/4)", 1.25),
        ("sqrt(1/4)", 0.5),
        ("2^(1/2)", 2.0_f64.sqrt()),
    ] {
        close(
            CompiledSource::new(&formula(text))?.value(0.0, 0.0)?,
            expected,
        );
    }
    Ok(())
}

#[test]
fn whitelisted_math_and_two_argument_functions_use_real_values() -> volna::Result<()> {
    for (text, expected) in [
        ("sin(pi/2)+cos(0)", 2.0),
        ("ln(exp(1))+log(exp(1))", 2.0),
        ("min(s, 1/2)+max(s, 3/4)", 1.0),
        ("pow(4, 1/2)+hypot(3,4)", 7.0),
        ("atan2(1,1)", PI / 4.0),
        ("abs(-3/2)+sqrt(1/4)", 2.0),
    ] {
        close(
            CompiledSource::new(&formula(text))?.value(0.0, 0.25)?,
            expected,
        );
    }
    Ok(())
}

#[test]
fn phase_changes_the_carrier_across_the_aperture() -> volna::Result<()> {
    let mut source = formula("1");
    source.signal = Signal::Continuous { ramp_periods: 1.0 };
    source.phase_profile = Profile::Formula {
        expression: "pi*s/2".into(),
    };
    let compiled = CompiledSource::new(&source)?;
    close(compiled.value(1.0, -1.0)?, -1.0);
    close(compiled.value(1.0, 0.0)?, 0.0);
    close(compiled.value(1.0, 1.0)?, 1.0);
    Ok(())
}

#[test]
fn formula_variables_delay_current_and_spatial_phase() -> volna::Result<()> {
    let mut source = formula("t + s + f + phi + pi");
    source.current_a_m2 = 2.0;
    source.delay_s = 0.25;
    source.phase_rad = 0.5;
    source.phase_profile = Profile::Formula {
        expression: "s/2".into(),
    };
    close(
        CompiledSource::new(&source)?.value(1.0, 0.5)?,
        2.0 * (0.75 + 0.5 + 1.0 + 0.75 + PI),
    );
    Ok(())
}

#[test]
fn cw_ramp_and_phase_are_not_an_envelope_delay() -> volna::Result<()> {
    let mut source = formula("1");
    source.signal = Signal::Continuous { ramp_periods: 1.0 };
    source.phase_rad = FRAC_PI_2;
    let compiled = CompiledSource::new(&source)?;
    close(compiled.value(0.0, 0.0)?, 0.0);
    close(compiled.value(0.5, 0.0)?, -0.5);
    close(compiled.value(1.0, 0.0)?, 1.0);
    source.phase_profile = Profile::Uniform;
    close(
        CompiledSource::new(&source)?.value(1.0, 0.0)?,
        1.0_f64.cos(),
    );
    Ok(())
}

#[test]
fn cw_ramp_preserves_small_nonzero_amplitudes() -> volna::Result<()> {
    let mut source = formula("1");
    source.signal = Signal::Continuous { ramp_periods: 1.0 };
    source.phase_rad = FRAC_PI_2;
    let time = 1e-10;
    let actual = CompiledSource::new(&source)?.value(time, 0.0)?;
    // Первый член ряда raised-cosine: относительная поправка порядка t² < 1e-19.
    let expected = PI * PI * time * time / 4.0;
    assert!(actual > 0.0);
    assert!((actual / expected - 1.0).abs() < 1e-12);
    Ok(())
}

#[test]
fn gaussian_carrier_has_independent_width_center_and_phase() -> volna::Result<()> {
    let mut source = formula("1");
    source.signal = Signal::Gaussian {
        center_s: 1.0,
        width_s: 0.5,
    };
    source.phase_rad = FRAC_PI_2;
    let compiled = CompiledSource::new(&source)?;
    close(compiled.value(1.0, 0.0)?, 1.0);
    close(compiled.value(1.5, 0.0)?, -(-0.5_f64).exp());
    Ok(())
}

#[test]
fn apertures_match_analytic_profiles() -> volna::Result<()> {
    let mut source = formula("1");
    source.aperture = Profile::Fundamental;
    close(CompiledSource::new(&source)?.value(0.0, 0.0)?, 1.0);
    close(CompiledSource::new(&source)?.value(0.0, 1.0)?, 0.0);
    source.aperture = Profile::Gaussian { width: 0.5 };
    close(
        CompiledSource::new(&source)?.value(0.0, 0.5)?,
        (-0.5_f64).exp(),
    );
    source.aperture = Profile::Formula {
        expression: "cos(pi*s/2)".into(),
    };
    close(
        CompiledSource::new(&source)?.value(0.0, 0.5)?,
        (PI / 4.0).cos(),
    );
    source.aperture = Profile::Table {
        samples: vec![[-1.0, 0.0], [0.0, 1.0], [1.0, 0.0]],
    };
    close(CompiledSource::new(&source)?.value(0.0, 0.25)?, 0.75);
    Ok(())
}

#[test]
fn temporal_tables_interpolate_zero_outside_and_ignore_phase() -> volna::Result<()> {
    let mut source = formula("1");
    source.signal = Signal::Table {
        samples: vec![[1.0, 0.0], [2.0, 1.0], [3.0, 0.0]],
        periodic: false,
    };
    source.phase_rad = 123.0;
    let compiled = CompiledSource::new(&source)?;
    close(compiled.value(0.5, 0.0)?, 0.0);
    close(compiled.value(1.5, 0.0)?, 0.5);
    close(compiled.value(3.5, 0.0)?, 0.0);
    source.signal = Signal::Table {
        samples: vec![[0.0, 0.0], [1.0, 1.0], [2.0, 0.0]],
        periodic: true,
    };
    close(CompiledSource::new(&source)?.value(2.5, 0.0)?, 0.5);
    Ok(())
}

#[test]
fn invalid_tables_and_parameters_are_rejected() {
    for samples in [
        vec![],
        vec![[0.0, 1.0]],
        vec![[-1.0, 0.0], [-1.0, 1.0]],
        vec![[0.0, 1.0], [1.0, 1.0]],
        vec![[-1.0, f64::NAN], [1.0, 1.0]],
        vec![[f64::NEG_INFINITY, 0.0], [1.0, 1.0]],
        (0..4097).map(|i| [i as f64 / 1024.0 - 1.0, 1.0]).collect(),
    ] {
        let mut source = formula("1");
        source.aperture = Profile::Table { samples };
        assert!(CompiledSource::new(&source).is_err());
    }
    for signal in [
        Signal::Continuous { ramp_periods: 0.0 },
        Signal::Continuous {
            ramp_periods: f64::NAN,
        },
        Signal::Gaussian {
            center_s: -1.0,
            width_s: 1.0,
        },
        Signal::Gaussian {
            center_s: 0.0,
            width_s: 0.0,
        },
        Signal::Table {
            samples: vec![[0.0, 0.0], [1.0, 1.0]],
            periodic: true,
        },
    ] {
        let mut source = formula("1");
        source.signal = signal;
        assert!(CompiledSource::new(&source).is_err());
    }
    let mut source = formula("1");
    source.current_a_m2 = f64::NAN;
    assert!(CompiledSource::new(&source).is_err());
    source.current_a_m2 = 1.0;
    source.aperture = Profile::Gaussian { width: 0.0 };
    assert!(CompiledSource::new(&source).is_err());
}

#[test]
fn expression_sandbox_rejects_side_effects_and_excessive_complexity() {
    for expression in [
        "t=2",
        "1;2",
        "\"text\"",
        "random()",
        "file(1)",
        "unknown + 1",
        "true",
        "1,2",
        "sin()",
        "sin(1,2)",
        "1e999",
        "",
    ] {
        assert!(
            CompiledSource::new(&formula(expression)).is_err(),
            "{expression}"
        );
    }
    for expression in [
        format!("{}1{}", "(".repeat(33), ")".repeat(33)),
        format!("{}1", "-".repeat(600)),
        "1+".repeat(1025),
        (0..100).map(|_| "1").collect::<Vec<_>>().join("+"),
    ] {
        assert!(CompiledSource::new(&formula(&expression)).is_err());
    }
}

#[test]
fn runtime_domain_and_nonfinite_input_errors_are_explicit() -> volna::Result<()> {
    for expression in ["sqrt(s)", "ln(s)", "1/s", "exp(1000*t)"] {
        let compiled = CompiledSource::new(&formula(expression))?;
        assert!(
            compiled
                .value(1.0, if expression == "sqrt(s)" { -1.0 } else { 0.0 })
                .is_err()
        );
    }
    let compiled = CompiledSource::new(&formula("1"))?;
    assert!(compiled.value(f64::NAN, 0.0).is_err());
    assert!(compiled.value(0.0, f64::INFINITY).is_err());
    assert!(compiled.value(0.0, 1.01).is_err());
    assert!(compiled.cost() > 0);
    Ok(())
}

#[test]
fn valid_large_tables_and_extreme_finite_interpolation() -> volna::Result<()> {
    let mut source = formula("1");
    source.aperture = Profile::Table {
        samples: (0..4096)
            .map(|i| [-1.0 + 2.0 * i as f64 / 4095.0, 0.5])
            .collect(),
    };
    close(CompiledSource::new(&source)?.value(0.0, 0.321)?, 0.5);
    source.aperture = Profile::Table {
        samples: vec![[-1.0, -f64::MAX], [1.0, f64::MAX]],
    };
    close(CompiledSource::new(&source)?.value(0.0, 0.0)?, 0.0);
    Ok(())
}

#[test]
fn invalid_constant_domains_fail_before_runtime() {
    for text in ["1/0", "sqrt(-1)", "ln(0)", "exp(1000)", "(-1)^(1/2)"] {
        assert!(CompiledSource::new(&formula(text)).is_err(), "{text}");
    }
}

#[test]
fn source_disabled_or_not_started_is_zero() -> volna::Result<()> {
    let mut source = formula("1");
    source.delay_s = 2.0;
    close(CompiledSource::new(&source)?.value(1.0, 0.0)?, 0.0);
    source.enabled = false;
    close(CompiledSource::new(&source)?.value(3.0, 0.0)?, 0.0);
    Ok(())
}
