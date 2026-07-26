//! The uniffi binding generator, used to emit Python (and later Swift/Kotlin)
//! bindings from the compiled cdylib. Built only with `--features ffi`.
fn main() {
    uniffi::uniffi_bindgen_main()
}
