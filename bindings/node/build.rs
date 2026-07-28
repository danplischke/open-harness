fn main() {
    // Sets the linker flags so the addon's undefined Node C-API symbols are
    // resolved at load time (cross-platform).
    napi_build::setup();
}
