/// `url` with its password, if it carries one, replaced the way gix
/// displays it. Anything else, a local path included, is left as typed,
/// since only the password is a secret and re-spelling could change it.
pub fn redact_password(url: &str) -> std::borrow::Cow<'_, str> {
    gix::Url::from_bytes(url.as_bytes().into())
        .ok()
        .filter(|parsed| parsed.password().is_some())
        .map_or(url.into(), |parsed| parsed.to_string().into())
}
