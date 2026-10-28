//! Search normalization for Arabic and Persian: drop vowel and Quranic marks and
//! tatweel, and fold letters that are written differently but searched alike.

pub fn normalize(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\u{0610}'..='\u{061A}' | '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{06D6}'..='\u{06ED}' | '\u{0640}' => None,
            'ٱ' | 'أ' | 'إ' | 'آ' => Some('ا'),
            'ى' | 'ی' => Some('ي'),
            'ک' => Some('ك'),
            c => Some(c),
        })
        .collect()
}
