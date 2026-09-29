// Not a runtime/spinel/test file: that lane loads only the CRuby overlay, so the spinel parser would go untested.

use std::path::Path;
use std::process::Command;

fn run_driver(implementation: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/time_parse_runtime.rb"))
        .arg(root.join(implementation))
        .env("TZ", "Asia/Tokyo")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("cases "),
        "driver failed before checking anything\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
    assert!(
        stdout.contains("ALL OK") && out.status.success(),
        "{implementation} diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
}

#[test]
fn the_spinel_parser_answers_like_activesupport() {
    run_driver("runtime/spinel/active_support_time_parsing.rb");
}

#[test]
fn the_cruby_overlay_answers_like_activesupport() {
    run_driver("runtime/spinel/scaffold/ruby_overlay/runtime/active_support_time_parsing.rb");
}

#[test]
fn a_shape_the_spinel_parser_does_not_know_raises_instead_of_guessing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg("-e")
        .arg(
            "load ARGV[0]; begin; ActiveSupport.zone_parse('Mon May 28 2012 00:00:00 GMT-0700 (PDT)'); \
             puts 'parsed'; rescue ArgumentError => e; puts e.message; end",
        )
        .arg(root.join("runtime/spinel/active_support_time_parsing.rb"))
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("unsupported time format"), "got: {stdout}");
}
