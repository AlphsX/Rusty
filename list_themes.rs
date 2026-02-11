use syntect::highlighting::ThemeSet;
fn main() {
    let ts = ThemeSet::load_defaults();
    for name in ts.themes.keys() {
        println!("{}", name);
    }
}
