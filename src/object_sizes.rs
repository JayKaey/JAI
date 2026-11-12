//! How large things are (2026-11-12). A model downloaded from the web carries no trustworthy
//! unit (an apple came 79 cm tall), so the 3D studio takes its size from the kind of thing Sir
//! asked for: the largest side of the model becomes the largest side of that thing. For a kind
//! that is not in the table no size is stated at all.

/// The largest side of everyday things, in metres, by the word Sir calls them. Longer names
/// first where one contains another ("tennis ball" before "ball").
const SIZES: &[(&str, f32)] = &[
    // Fruit and food.
    ("apple", 0.08), ("banana", 0.19), ("orange", 0.075), ("pear", 0.1), ("lemon", 0.07), ("strawberry", 0.035), ("pineapple", 0.3), ("watermelon", 0.3), ("cherry", 0.022),
    ("grape", 0.02), ("peach", 0.07), ("mango", 0.12), ("coconut", 0.14), ("avocado", 0.1), ("tomato", 0.07), ("potato", 0.1), ("carrot", 0.18), ("pumpkin", 0.35), ("onion", 0.08),
    ("burger", 0.11), ("hamburger", 0.11), ("pizza", 0.3), ("cake", 0.22), ("donut", 0.09), ("doughnut", 0.09), ("cookie", 0.07), ("egg", 0.06), ("bread", 0.3), ("croissant", 0.14),
    ("wine glass", 0.21), ("coffee cup", 0.09), ("cup", 0.09), ("mug", 0.1), ("bottle", 0.26), ("can", 0.12), ("teapot", 0.25), ("kettle", 0.25), ("plate", 0.26), ("bowl", 0.16),
    ("fork", 0.19), ("spoon", 0.17), ("knife", 0.24), ("pan", 0.45),
    // Animals.
    ("cat", 0.46), ("kitten", 0.25), ("dog", 0.8), ("puppy", 0.35), ("horse", 2.4), ("cow", 2.4), ("pig", 1.5), ("sheep", 1.3), ("goat", 1.2), ("elephant", 6.0), ("giraffe", 5.2),
    ("lion", 2.2), ("tiger", 2.6), ("bear", 2.2), ("wolf", 1.5), ("fox", 0.9), ("deer", 1.8), ("rabbit", 0.4), ("bunny", 0.4), ("hamster", 0.12), ("mouse", 0.1), ("rat", 0.24),
    ("squirrel", 0.25), ("monkey", 0.6), ("gorilla", 1.7), ("panda", 1.6), ("koala", 0.75), ("kangaroo", 1.5), ("bird", 0.18), ("eagle", 2.0), ("owl", 0.45), ("parrot", 0.4),
    ("duck", 0.55), ("chicken", 0.45), ("penguin", 0.9), ("fish", 0.25), ("shark", 4.5), ("whale", 25.0), ("dolphin", 2.6), ("octopus", 1.0), ("turtle", 0.5), ("frog", 0.09),
    ("snake", 1.5), ("lizard", 0.3), ("crocodile", 4.0), ("dinosaur", 12.0), ("t-rex", 12.0), ("dragon", 12.0), ("spider", 0.06), ("butterfly", 0.08), ("bee", 0.015), ("ant", 0.008),
    // People and their parts.
    ("astronaut", 1.9), ("skeleton", 1.75), ("human", 1.75), ("person", 1.75), ("man", 1.78), ("woman", 1.66), ("child", 1.2), ("baby", 0.55), ("robot", 1.7), ("knight", 1.85),
    ("skull", 0.2), ("hand", 0.19), ("heart", 0.12), ("brain", 0.17), ("tooth", 0.022), ("eye", 0.025), ("head", 0.24),
    // Vehicles.
    ("race car", 4.6), ("sports car", 4.5), ("car", 4.5), ("truck", 7.5), ("bus", 12.0), ("van", 5.2), ("motorcycle", 2.1), ("motorbike", 2.1), ("bicycle", 1.8), ("bike", 1.8),
    ("scooter", 1.1), ("skateboard", 0.8), ("train", 25.0), ("locomotive", 20.0), ("tram", 30.0), ("airplane", 38.0), ("aeroplane", 38.0), ("plane", 38.0), ("jet", 16.0),
    ("helicopter", 13.0), ("rocket", 55.0), ("space shuttle", 37.0), ("satellite", 5.0), ("ship", 150.0), ("boat", 7.0), ("sailboat", 9.0), ("submarine", 80.0), ("tank", 9.5),
    ("tractor", 4.5), ("drone", 0.35), ("ufo", 12.0),
    // Furniture and the house.
    ("chair", 0.95), ("armchair", 0.95), ("stool", 0.6), ("table", 1.6), ("desk", 1.4), ("sofa", 2.1), ("couch", 2.1), ("bed", 2.05), ("lamp", 0.5), ("bookshelf", 1.9),
    ("shelf", 1.2), ("wardrobe", 2.0), ("door", 2.05), ("window", 1.3), ("toilet", 0.75), ("bathtub", 1.7), ("sink", 0.6), ("fridge", 1.8), ("oven", 0.6), ("washing machine", 0.85),
    ("mirror", 0.8), ("clock", 0.3), ("vase", 0.3), ("candle", 0.15), ("lantern", 0.3), ("barrel", 0.9), ("crate", 0.5), ("box", 0.4), ("chest", 0.75), ("bucket", 0.3),
    // Buildings.
    ("eiffel tower", 330.0), ("statue of liberty", 93.0), ("skyscraper", 300.0), ("lighthouse", 30.0), ("windmill", 25.0), ("castle", 60.0), ("church", 40.0), ("pyramid", 230.0),
    ("house", 12.0), ("cabin", 7.0), ("tent", 2.5), ("bridge", 300.0), ("tower", 60.0), ("barn", 15.0),
    // Devices.
    ("smartphone", 0.15), ("iphone", 0.15), ("phone", 0.15), ("ipad", 0.25), ("tablet", 0.25), ("laptop", 0.36), ("keyboard", 0.44), ("monitor", 0.62), ("television", 1.25), ("tv", 1.25),
    ("computer", 0.45), ("camera", 0.14), ("headphones", 0.2), ("headset", 0.2), ("microphone", 0.2), ("speaker", 0.3), ("controller", 0.16), ("gamepad", 0.16), ("console", 0.3),
    ("watch", 0.045), ("radio", 0.3), ("telescope", 1.0), ("microscope", 0.4), ("printer", 0.45), ("router", 0.22),
    // Tools and weapons.
    ("hammer", 0.33), ("screwdriver", 0.2), ("wrench", 0.22), ("pliers", 0.18), ("saw", 0.5), ("drill", 0.25), ("axe", 0.7), ("shovel", 1.1), ("ladder", 2.5), ("sword", 1.0),
    ("katana", 1.0), ("dagger", 0.3), ("shield", 0.75), ("pistol", 0.2), ("gun", 0.2), ("rifle", 1.0), ("bow", 1.5), ("arrow", 0.75), ("spear", 2.0), ("lightsaber", 1.1),
    // Music and play.
    ("guitar", 1.0), ("violin", 0.6), ("piano", 1.5), ("drum", 0.5), ("trumpet", 0.5), ("flute", 0.66), ("tennis ball", 0.067), ("golf ball", 0.043), ("basketball", 0.24),
    ("football", 0.22), ("soccer ball", 0.22), ("baseball", 0.074), ("ball", 0.22), ("chess piece", 0.08), ("chess board", 0.45), ("dice", 0.016), ("die", 0.016), ("lego brick", 0.032),
    ("teddy bear", 0.35), ("teddy", 0.35), ("doll", 0.3), ("kite", 1.0), ("balloon", 0.3),
    // Things to wear and carry.
    ("backpack", 0.5), ("bag", 0.4), ("shoe", 0.3), ("sneaker", 0.3), ("boot", 0.32), ("hat", 0.32), ("cap", 0.28), ("helmet", 0.3), ("mask", 0.25), ("glasses", 0.14),
    ("sunglasses", 0.14), ("crown", 0.2), ("ring", 0.022), ("necklace", 0.25), ("umbrella", 1.0), ("wallet", 0.11), ("key", 0.06), ("coin", 0.025), ("book", 0.24), ("pen", 0.14),
    ("pencil", 0.19), ("trophy", 0.35), ("medal", 0.07),
    // Nature.
    ("tree", 9.0), ("palm tree", 9.0), ("christmas tree", 2.0), ("bush", 1.2), ("flower", 0.3), ("rose", 0.45), ("sunflower", 1.8), ("tulip", 0.4), ("cactus", 0.6), ("plant", 0.45),
    ("mushroom", 0.08), ("leaf", 0.1), ("rock", 0.4), ("stone", 0.15), ("crystal", 0.1), ("diamond", 0.012), ("gem", 0.015), ("snowman", 1.5), ("snowflake", 0.004), ("shell", 0.08),
    // Machines and parts.
    ("engine", 0.8), ("wheel", 0.65), ("tire", 0.65), ("tyre", 0.65), ("gear", 0.1), ("bolt", 0.04), ("screw", 0.03), ("spring", 0.06), ("turbine", 3.0), ("propeller", 2.0),
    ("robot arm", 1.2), ("battery", 0.05), ("light bulb", 0.11), ("lightbulb", 0.11), ("magnet", 0.08), ("anchor", 1.5), ("bell", 0.3), ("compass", 0.07), ("globe", 0.33),
];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '-').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// A word without its plural ("apples" → "apple", "boxes" → "box", "knives" stays as it is).
fn singular(word: &str) -> String {
    if word.len() > 4 && word.ends_with("ies") {
        format!("{}y", &word[..word.len() - 3])
    } else if word.len() > 3 && (word.ends_with("xes") || word.ends_with("ches") || word.ends_with("shes") || word.ends_with("sses")) {
        word[..word.len() - 2].to_string()
    } else if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") && !word.ends_with("us") {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

/// The largest side (metres) of the thing `request` asks for ("show me a 3D model of an
/// apple", "a red sports car"): the thing named last (an "apple tree" is a tree), a name of two
/// or three words before one of one. None for a thing not in the table.
pub fn typical_size_m(request: &str) -> Option<f32> {
    let w: Vec<String> = words(request).iter().map(|w| singular(w)).collect();
    let mut best: Option<(usize, usize, f32)> = None;
    for (name, size) in SIZES {
        let parts: Vec<&str> = name.split(' ').collect();
        for at in 0..w.len().saturating_sub(parts.len() - 1) {
            if w[at..at + parts.len()].iter().zip(&parts).all(|(a, b)| a == b) {
                let end = at + parts.len();
                // The one that ends last wins; of two that end at the same word, the longer name.
                if best.is_none_or(|(e, n, _)| end > e || (end == e && parts.len() > n)) {
                    best = Some((end, parts.len(), *size));
                }
            }
        }
    }
    best.map(|(_, _, size)| size)
}

/// A downloaded model's real size: `measured` (width, height, depth in its own, untrusted
/// unit) scaled so that its largest side is that of the thing `request` asks for. None when
/// the thing is not known (then no size is stated).
pub fn real_size(request: &str, measured: [f32; 3]) -> Option<[f32; 3]> {
    let largest = measured.iter().copied().fold(0.0f32, f32::max);
    let size = typical_size_m(request)?;
    (largest > 0.0 && largest.is_finite()).then(|| measured.map(|side| side * size / largest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_apple_is_an_apple_and_an_apple_tree_a_tree() {
        assert_eq!(typical_size_m("Show me a 3D model of an apple"), Some(0.08));
        assert_eq!(typical_size_m("apples"), Some(0.08));
        assert_eq!(typical_size_m("an apple tree"), Some(9.0));
        assert_eq!(typical_size_m("a red sports car"), Some(4.5));
        assert_eq!(typical_size_m("tennis ball"), Some(0.067), "the longer name, not just a ball");
        assert_eq!(typical_size_m("the Eiffel Tower"), Some(330.0));
        assert_eq!(typical_size_m("an iPad"), Some(0.25));
        assert_eq!(typical_size_m("two boxes"), Some(0.4));
        assert_eq!(typical_size_m("a flux capacitor"), None, "what he has no size for gets none");
        assert_eq!(typical_size_m(""), None);
        assert_eq!(typical_size_m("a dog on a skateboard"), Some(0.8), "the thing named last");
    }

    #[test]
    fn a_model_takes_the_size_of_the_thing_it_shows() {
        // The apple that came 79 cm tall.
        let apple = real_size("show me a 3D model of an apple", [0.7, 0.79, 0.71]).unwrap();
        assert!((apple[1] - 0.08).abs() < 1e-4 && apple[0] < 0.08 && apple[2] < 0.08, "{apple:?}");
        // A car lying along x keeps its proportions.
        let car = real_size("a car", [9.0, 3.0, 4.0]).unwrap();
        assert!((car[0] - 4.5).abs() < 1e-4 && (car[1] - 1.5).abs() < 1e-4);
        assert_eq!(real_size("a thingamajig", [1.0, 1.0, 1.0]), None);
        assert_eq!(real_size("an apple", [0.0, 0.0, 0.0]), None);
    }

    #[test]
    fn every_size_is_plausible_and_no_name_is_listed_twice() {
        let mut names: Vec<&str> = SIZES.iter().map(|(name, _)| *name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a name twice");
        assert!(SIZES.iter().all(|(name, size)| *size > 0.001 && *size < 500.0 && !name.is_empty() && *name == name.to_lowercase()));
        assert_eq!(singular("berries"), "berry");
        assert_eq!(singular("glass"), "glass");
        assert_eq!(singular("cactus"), "cactus");
    }
}
