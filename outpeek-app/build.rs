fn main() {
    println!("cargo::rerun-if-changed=ui/");
    // `--features ui-debug` turns on Slint's element metadata so the embedded
    // MCP server can address widgets (get_element_tree/click_element).
    // Production builds leave it off: it embeds widget paths and geometry.
    if std::env::var_os("CARGO_FEATURE_UI_DEBUG").is_some() {
        std::env::set_var("SLINT_EMIT_DEBUG_INFO", "1");
        println!("cargo::rustc-cfg=ui_debug");
    }
    // Tokens/components are shared with the HUD crate, never copied: the
    // include path makes `tokens.slint` and `components/plate.slint` resolve
    // out of `outpeek-overlay/ui`.
    println!("cargo::rerun-if-changed=../outpeek-overlay/ui/");
    // The Settings page reports the target triple the compiler actually built
    // for. Guessing it from `std::env::consts` would be a different string.
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo::rustc-env=OUTPEEK_TARGET={target}");
    }
    // Edition 1.19: `if`/`for` children are sized like plain children and a
    // state-only property keeps its default. Opted in deliberately — it is the
    // behaviour every new Slint release documents going forward.
    let config = slint_build::CompilerConfiguration::new()
        .with_edition((1, 19))
        .with_include_paths(vec![std::path::PathBuf::from("../outpeek-overlay/ui")]);
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("outpeek-app: ui/app.slint must compile");
}
