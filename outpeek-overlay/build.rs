fn main() {
    println!("cargo::rerun-if-changed=ui/");
    // Edition 1.19: `if`/`for` children are sized like plain children and a
    // state-only property keeps its default. Opted in deliberately — it is the
    // behaviour every new Slint release documents going forward.
    let config = slint_build::CompilerConfiguration::new().with_edition((1, 19));
    slint_build::compile_with_config("ui/overlay.slint", config)
        .expect("outpeek-overlay: ui/overlay.slint must compile");
}
