//! Type 1 subset glyph availability (ISO 32000-1 Table 122, `/CharSet`). Widths and
//! ToUnicode can describe the full face even when its embedded subset lacks an outline.

use super::*;

// A simple font has at most 256 encoded glyphs, but its subset can list unencoded glyphs too.
// Bound both the string and its tokens before parsing any input-derived objects.
const MAX_CHARSET_BYTES: usize = 64 * 1024;
const MAX_CHARSET_NAMES: usize = 4096;
const MAX_TYPE1_BYTES: usize = 4 * 1024 * 1024;

fn charset(doc: &Document, font: &Dict) -> Option<HashSet<Vec<u8>>> {
    let descriptor = dict(doc, font.get(b"FontDescriptor"))?;
    let value = doc.resolve(descriptor.get(b"CharSet")?);
    let Object::String(value) = value.as_ref() else { return None };
    if value.bytes.len() > MAX_CHARSET_BYTES {
        return None;
    }
    let mut lexer = pdfcraft_cos::Lexer::new(&value.bytes, 0);
    let mut names = HashSet::new();
    // One more pass than names: the last one only finds the end of the string.
    for _ in 0..=MAX_CHARSET_NAMES {
        lexer.skip_ws();
        if lexer.pos == value.bytes.len() {
            return (!names.is_empty()).then_some(names);
        }
        if names.len() == MAX_CHARSET_NAMES {
            return None;
        }
        // Do not let a corrupt name list build arbitrary nested PDF objects.
        if value.bytes.get(lexer.pos) != Some(&b'/') {
            return None;
        }
        let Object::Name(name) = lexer.object().ok()? else { return None };
        if name.is_empty() {
            return None;
        }
        names.insert(name);
    }
    None
}

/// Without an explicit base encoding, glyph names come from the font program, not
/// StandardEncoding or ToUnicode. Read it through the existing safe font parser.
fn builtin_presence(doc: &Document, font: &Dict, names: &HashSet<Vec<u8>>) -> Option<[Option<bool>; 256]> {
    let descriptor = dict(doc, font.get(b"FontDescriptor"))?;
    let program = doc.resolve(descriptor.get(b"FontFile")?);
    let Object::Stream(program) = program.as_ref() else { return None };
    let bytes = program.decoded_within(MAX_TYPE1_BYTES).ok()?;
    let font = skrifa::raw::ps::type1::Type1Font::new(&bytes).ok()?;
    let encoding = font.encoding()?;
    let mut present = [None; 256];
    for (code, slot) in present.iter_mut().enumerate() {
        // This fixed 256-entry table makes every code fit in one byte.
        *slot = match encoding.map(code as u8) {
            Some(gid) if gid != skrifa::GlyphId::NOTDEF => font.glyph_name(gid).map(|name| names.contains(name.as_bytes())),
            _ => Some(false),
        };
    }
    Some(present)
}

pub(super) fn absent_codes(doc: &Document, font: &Dict) -> [u64; 4] {
    let mut absent = [0_u64; 4];
    if !matches!(font.name(b"Subtype"), Some(b"Type1" | b"MMType1")) {
        return absent;
    }
    let Some(names) = charset(doc, font) else { return absent };
    let encoding = font.get(b"Encoding").map(|e| doc.resolve(e));
    let base = match encoding.as_deref() {
        Some(Object::Name(name)) => Some(name.as_slice()),
        Some(Object::Dict(d)) => d.name(b"BaseEncoding"),
        _ => None,
    };
    let table = match base {
        Some(b"WinAnsiEncoding") => Some(&crate::encodings::WIN_ANSI),
        Some(b"MacRomanEncoding") => Some(&crate::encodings::MAC_ROMAN),
        Some(b"StandardEncoding") => Some(&crate::encodings::STANDARD),
        _ => None,
    };
    // Annex D's tables and NAMES use the same canonical glyph names. This is the base
    // encoding's glyph identity, never the extraction text supplied by ToUnicode.
    let canonical: HashMap<_, _> = crate::encodings::NAMES.iter().map(|(name, unicode)| (*unicode, name.as_bytes())).collect();
    let mut present = if base.is_none() { builtin_presence(doc, font, &names).unwrap_or([None; 256]) } else { [None; 256] };
    for (code, unicode) in table.into_iter().flatten().enumerate() {
        if let Some(slot) = present.get_mut(code) {
            *slot = canonical.get(unicode).map(|name| names.contains(*name));
        }
    }
    if let Some(Object::Dict(d)) = encoding.as_deref()
        && let Some(diffs) = d.get(b"Differences").map(|v| doc.resolve(v))
        && let Some(diffs) = diffs.as_array()
    {
        let mut code = 0_u32;
        for item in diffs {
            // An entry may be an indirect reference; skipping it would shift every later code.
            match doc.resolve(item).as_ref() {
                Object::Int(n) => code = u32::try_from(*n).unwrap_or(u32::MAX),
                Object::Name(name) => {
                    if let Some(slot) = present.get_mut(code as usize) {
                        *slot = Some(name != b".notdef" && names.contains(name));
                    }
                    code = code.saturating_add(1);
                }
                _ => {}
            }
        }
    }
    for (code, value) in present.iter().enumerate() {
        if *value == Some(false)
            && let Some(word) = absent.get_mut(code / 64)
        {
            *word |= 1_u64 << (code % 64);
        }
    }
    absent
}
