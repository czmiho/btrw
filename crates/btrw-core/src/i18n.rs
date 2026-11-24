use fluent::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

const EN_FTL: &str = include_str!("../locales/en.ftl");
const CS_FTL: &str = include_str!("../locales/cs.ftl");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    Cs,
}

impl Locale {
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Cs => "cs",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        let normalized = tag.trim().to_ascii_lowercase();
        let primary = normalized
            .split(['.', '@', '_', '-'])
            .next()
            .unwrap_or_default();
        match primary {
            "en" | "c" | "posix" => Some(Locale::En),
            "cs" | "cz" => Some(Locale::Cs),
            _ => None,
        }
    }

    /// Resolution order: CLI override -> config language -> environment -> English fallback.
    pub fn resolve(cli_override: Option<&str>, config_lang: Option<&str>) -> Self {
        if let Some(tag) = cli_override
            && !tag.eq_ignore_ascii_case("auto")
            && let Some(locale) = Self::from_tag(tag)
        {
            return locale;
        }
        if let Some(tag) = config_lang
            && !tag.eq_ignore_ascii_case("auto")
            && let Some(locale) = Self::from_tag(tag)
        {
            return locale;
        }
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(var)
                && !value.is_empty()
                && let Some(locale) = Self::from_tag(&value)
            {
                return locale;
            }
        }
        Locale::En
    }
}

pub struct Catalog {
    locale: Locale,
    active: FluentBundle<FluentResource>,
    fallback: FluentBundle<FluentResource>,
}

impl Catalog {
    pub fn new(locale: Locale) -> Self {
        let fallback = build_bundle("en", EN_FTL);
        let active = match locale {
            Locale::En => build_bundle("en", EN_FTL),
            Locale::Cs => build_bundle("cs", CS_FTL),
        };
        Self {
            locale,
            active,
            fallback,
        }
    }

    pub fn locale(&self) -> Locale {
        self.locale
    }

    pub fn t(&self, key: &str) -> String {
        self.format(key, None)
    }

    pub fn t_args<'a>(&self, key: &str, pairs: &[(&'a str, FluentValue<'a>)]) -> String {
        let mut args = FluentArgs::new();
        for (name, value) in pairs {
            args.set(*name, value.clone());
        }
        self.format(key, Some(&args))
    }

    fn format(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        if let Some(rendered) = format_in(&self.active, key, args) {
            return rendered;
        }
        if let Some(rendered) = format_in(&self.fallback, key, args) {
            return rendered;
        }
        key.to_string()
    }
}

fn build_bundle(lang: &str, source: &str) -> FluentBundle<FluentResource> {
    let langid: LanguageIdentifier = lang.parse().expect("valid static langid");
    let mut bundle = FluentBundle::new(vec![langid]);
    // Disable Unicode bidi isolation markers so terminal column alignment stays exact.
    bundle.set_use_isolating(false);
    let resource = FluentResource::try_new(source.to_string())
        .expect("embedded Fluent resource must parse cleanly");
    bundle
        .add_resource(resource)
        .expect("embedded Fluent resource has no duplicate keys");
    bundle
}

fn format_in(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<String> {
    let msg = bundle.get_message(key)?;
    let pattern = msg.value()?;
    let mut errors = Vec::new();
    let value = bundle.format_pattern(pattern, args, &mut errors);
    Some(value.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn extract_keys(source: &str) -> BTreeSet<String> {
        source
            .lines()
            .filter_map(|line| {
                let first = line.chars().next()?;
                if !first.is_ascii_alphabetic() {
                    return None;
                }
                let (key, _) = line.split_once('=')?;
                Some(key.trim().to_string())
            })
            .collect()
    }

    #[test]
    fn english_and_czech_have_identical_keys() {
        let en = extract_keys(EN_FTL);
        let cs = extract_keys(CS_FTL);
        assert_eq!(en, cs, "Translation key mismatch between en.ftl and cs.ftl");
    }

    #[test]
    fn pluralization_works_without_bidi_isolates() {
        let en = Catalog::new(Locale::En);
        assert_eq!(en.t_args("snapshot-count", &[("count", 1.into())]), "1 snapshot");
        assert_eq!(en.t_args("snapshot-count", &[("count", 5.into())]), "5 snapshots");

        let cs = Catalog::new(Locale::Cs);
        assert_eq!(cs.t_args("snapshot-count", &[("count", 1.into())]), "1 snapshot");
        assert_eq!(cs.t_args("snapshot-count", &[("count", 3.into())]), "3 snapshoty");
        assert_eq!(cs.t_args("snapshot-count", &[("count", 5.into())]), "5 snapshotů");
    }

    #[test]
    fn locale_resolution_prefers_cli_over_config() {
        assert_eq!(Locale::resolve(Some("cs"), Some("en")), Locale::Cs);
        assert_eq!(Locale::resolve(Some("auto"), Some("cs")), Locale::Cs);
        assert_eq!(Locale::resolve(Some("en_US.UTF-8"), Some("cs")), Locale::En);
    }
}
