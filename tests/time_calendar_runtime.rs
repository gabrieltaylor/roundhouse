// Not a runtime/ruby/test file: those transpile to every target, and these functions ship to the ruby family only.

use std::path::Path;
use std::process::Command;

fn run_driver(time_parsing: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("ruby")
        .arg(root.join("tests/time_calendar_runtime.rb"))
        .arg(root.join(time_parsing))
        .arg(root.join("runtime/ruby/active_support_ext.rb"))
        .env("TZ", "America/New_York")
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
        "calendar functions diverged from activesupport\n=== stdout ===\n{stdout}\n=== stderr ===\n{stderr}"
    );
}

#[test]
fn the_calendar_functions_answer_like_activesupport_on_the_spinel_tree() {
    run_driver("runtime/spinel/active_support_time_parsing.rb");
}

#[test]
fn the_calendar_functions_answer_like_activesupport_on_the_cruby_tree() {
    run_driver("runtime/spinel/scaffold/ruby_overlay/runtime/active_support_time_parsing.rb");
}
