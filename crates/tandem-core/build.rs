fn main() {
    println!("cargo:rerun-if-changed=vendor/stretch_shim.cpp");
    println!("cargo:rerun-if-changed=vendor/signalsmith-stretch/signalsmith-stretch.h");
    let mut b = cc::Build::new();
    b.cpp(true)
        .file("vendor/stretch_shim.cpp")
        .include("vendor")
        .opt_level(3)
        .warnings(false);
    if b.get_compiler().is_like_msvc() {
        b.flag("/std:c++14").flag("/EHsc");
    } else {
        b.flag("-std=c++14");
    }
    b.compile("tandem_stretch");
}
