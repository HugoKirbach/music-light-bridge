//! Grave l'icône de DJ-Bridge dans les exécutables, sous Windows. Sans compilateur de ressources,
//! ils s'en passent : la compilation continue, avec un avertissement.

fn main() {
    println!("cargo:rerun-if-changed=assets/dj-bridge.rc");
    println!("cargo:rerun-if-changed=assets/dj-bridge.ico");
    if let Err(e) = embed_resource::compile("assets/dj-bridge.rc", embed_resource::NONE).manifest_optional() {
        println!("cargo:warning=icône non intégrée aux exécutables : {e}");
    }
}
