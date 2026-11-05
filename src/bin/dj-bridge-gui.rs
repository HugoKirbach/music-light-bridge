//! La fenêtre de DJ-Bridge, sans console : l'exécutable d'un double-clic (copié à la racine du
//! dossier sous le nom DJ-Bridge.exe, et visé par le raccourci du Bureau).

#![windows_subsystem = "windows"]

fn main() {
    if let Err(e) = dj_bridge::open_window() {
        // Sans console pour l'afficher : l'erreur va dans le dossier de configuration.
        let written = dj_bridge::config::Config::path()
            .map(|path| path.with_file_name("erreur-fenetre.txt"))
            .and_then(|path| Ok(std::fs::write(path, format!("{e:#}\n"))?));
        if written.is_err() {
            std::process::exit(1);
        }
    }
}
