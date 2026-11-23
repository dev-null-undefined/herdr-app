//! HERDR-WINDOWS: data drags (`ExternalDragPayload::Data`) between the
//! windows of one client: how a drag that enters one of our surfaces is read.

/// How a drag that entered one of our surfaces is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DragRead {
    /// The data drag this client started: as its own MIME type.
    Data(String),
    /// Anything else: as a file list (`text/uri-list`), as before.
    Files,
}

/// Chooses on Enter from the offer's MIME types: this client's own data drag
/// when the offer carries its type, else the file list as always. Another
/// client's data types are never read: they mean nothing here.
pub(crate) fn choose(offered: &[String], own: Option<&str>) -> DragRead {
    match own {
        Some(mime) if offered.iter().any(|m| m == mime) => DragRead::Data(mime.to_owned()),
        _ => DragRead::Files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(types: &[&str]) -> Vec<String> {
        types.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn our_own_data_drag_is_read_as_its_type_anything_else_as_files() {
        let herdr = "application/x-herdr-drag";
        assert_eq!(choose(&offered(&["text/plain", herdr]), Some(herdr)), DragRead::Data(herdr.into()));
        // Not ours (another client's, or ours ended): the file path, as before.
        assert_eq!(choose(&offered(&[herdr]), None), DragRead::Files);
        assert_eq!(choose(&offered(&["application/x-other"]), Some(herdr)), DragRead::Files);
        assert_eq!(choose(&offered(&["text/uri-list"]), Some(herdr)), DragRead::Files);
        assert_eq!(choose(&[], Some(herdr)), DragRead::Files);
    }
}
