//! Layout v1 paths. All persistent references are relative to one database root.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid storage path, database name or incarnation")]
pub struct InvalidPath;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseName(String);
impl DatabaseName {
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidPath> {
        let name = name.into();
        // MySQL quoted database identifiers; callers pass the literal name, without quotes.
        if !(1..=64).contains(&name.chars().count())
            || name.ends_with(' ')
            || name
                .chars()
                .any(|c| matches!(c, '\0' | '/' | '\\' | '.') || c as u32 > 0xffff)
        {
            return Err(InvalidPath);
        }
        Ok(Self(name))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One creation/restore lifecycle. Existing safe non-UUID identifiers remain valid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncarnationId(String);
impl IncarnationId {
    pub fn new(id: impl Into<String>) -> Result<Self, InvalidPath> {
        let id = id.into();
        RelativeKey::new(&id)?;
        if id.contains('/') {
            return Err(InvalidPath);
        }
        Ok(Self(id))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativeKey(String);
impl RelativeKey {
    pub fn new(key: impl Into<String>) -> Result<Self, InvalidPath> {
        let key = key.into();
        if key
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
            || !key
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c))
        {
            return Err(InvalidPath);
        }
        Ok(Self(key))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
    /// Interpret a safe relative key as a layout-v1 immutable object path.
    /// General relative keys (including head.json) need not have this shape.
    pub fn object_parts(&self) -> Option<ObjectKey<'_>> {
        let mut parts = self.0.splitn(6, '/');
        if parts.next()? != "incarnations" {
            return None;
        }
        let incarnation = IncarnationId::new(parts.next()?).ok()?;
        if parts.next()? != "generations" {
            return None;
        }
        let epoch = parts.next()?;
        if epoch.len() != 16
            || !epoch
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return None;
        }
        let epoch = u64::from_str_radix(epoch, 16).ok()?;
        if epoch == 0 {
            return None;
        }
        Some(ObjectKey {
            incarnation,
            epoch,
            category: parts.next()?,
            object: parts.next()?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectKey<'a> {
    pub incarnation: IncarnationId,
    pub epoch: u64,
    pub category: &'a str,
    pub object: &'a str,
}

pub fn object_key(
    incarnation: &IncarnationId,
    epoch: u64,
    category: &RelativeKey,
    object: &str,
) -> Result<RelativeKey, InvalidPath> {
    if epoch == 0 {
        return Err(InvalidPath);
    }
    let object = RelativeKey::new(object)?;
    RelativeKey::new(format!(
        "incarnations/{}/generations/{epoch:016x}/{}/{}",
        incarnation.as_str(),
        category.as_str(),
        object.as_str()
    ))
}
#[derive(Clone, Debug, Default)]
pub struct StorageNamespace {
    prefix: String,
}
impl StorageNamespace {
    pub fn new(prefix: &str) -> Result<Self, InvalidPath> {
        if prefix.is_empty() {
            return Ok(Self {
                prefix: String::new(),
            });
        }
        let prefix = prefix.strip_suffix('/').unwrap_or(prefix);
        Ok(Self {
            prefix: format!("{}/", RelativeKey::new(prefix)?.0),
        })
    }
    pub fn databases_prefix(&self) -> String {
        format!("{}databases/", self.prefix)
    }
    pub fn database(&self, name: DatabaseName) -> DatabasePath {
        DatabasePath {
            root: format!("{}{}/", self.databases_prefix(), name.as_str()),
            name,
        }
    }
    pub fn database_name(&self, prefix: &str) -> Result<DatabaseName, InvalidPath> {
        let root = self.databases_prefix();
        let name = prefix
            .strip_prefix(&root)
            .and_then(|s| s.strip_suffix('/'))
            .ok_or(InvalidPath)?;
        DatabaseName::new(name)
    }
}

/// Binds every resolved key to one validated database name and namespace.
#[derive(Clone, Debug)]
pub struct DatabasePath {
    root: String,
    name: DatabaseName,
}
impl DatabasePath {
    pub fn name(&self) -> &DatabaseName {
        &self.name
    }
    pub fn prefix(&self) -> &str {
        &self.root
    }
    pub fn head_key(&self) -> String {
        format!("{}head.json", self.root)
    }
    pub fn incarnations_prefix(&self) -> String {
        format!("{}incarnations/", self.root)
    }
    pub fn resolve(&self, key: &RelativeKey) -> String {
        format!("{}{}", self.root, key.as_str())
    }
    pub fn relative_key(&self, physical: &str) -> Result<RelativeKey, InvalidPath> {
        RelativeKey::new(physical.strip_prefix(&self.root).ok_or(InvalidPath)?)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_and_boundaries() {
        let ns = StorageNamespace::new("prod/").unwrap();
        assert_eq!(
            ns.database(DatabaseName::new("notes").unwrap()).head_key(),
            "prod/databases/notes/head.json"
        );
        for bad in [
            "../other",
            "/root",
            "a//b",
            "a/./b",
            "a\\b",
            "%2e%2e",
            "https://x",
            "a/",
        ] {
            assert!(RelativeKey::new(bad).is_err(), "{bad}");
        }
        for bad in ["", "a/b", "a.b", "a\\b", "a ", "a\0b", "😀"] {
            assert!(DatabaseName::new(bad).is_err());
        }
        for valid in [
            "a", "ABC", "_abc", "中文", "123", "$db", "a-b", "a b", "select",
        ] {
            assert!(DatabaseName::new(valid).is_ok(), "{valid}");
        }
        assert!(DatabaseName::new("中".repeat(64)).is_ok());
        assert!(DatabaseName::new("中".repeat(65)).is_err());
        assert!(!ns
            .database(DatabaseName::new("notes_old").unwrap())
            .prefix()
            .starts_with(ns.database(DatabaseName::new("notes").unwrap()).prefix()));
    }

    #[test]
    fn v1_golden_paths_and_components() {
        let namespace = StorageNamespace::new("prod/").unwrap();
        let database = namespace.database(DatabaseName::new("中文 $Notes-1").unwrap());
        let incarnation = IncarnationId::new("inc_001").unwrap();
        let key = object_key(
            &incarnation,
            42,
            &RelativeKey::new("delta-index").unwrap(),
            "object_001",
        )
        .unwrap();
        assert_eq!(
            key.as_str(),
            "incarnations/inc_001/generations/000000000000002a/delta-index/object_001"
        );
        assert_eq!(
            database.head_key(),
            "prod/databases/中文 $Notes-1/head.json"
        );
        assert_eq!(
            database.incarnations_prefix(),
            "prod/databases/中文 $Notes-1/incarnations/"
        );
        assert_eq!(database.resolve(&key), "prod/databases/中文 $Notes-1/incarnations/inc_001/generations/000000000000002a/delta-index/object_001");
        let parsed = key.object_parts().unwrap();
        assert_eq!(parsed.incarnation, incarnation);
        assert_eq!(parsed.epoch, 42);
        assert_eq!(parsed.category, "delta-index");
        assert_eq!(parsed.object, "object_001");
        assert_eq!(database.relative_key(&database.resolve(&key)).unwrap(), key);
        assert_eq!(
            namespace.database_name(database.prefix()).unwrap(),
            *database.name()
        );
    }

    #[test]
    fn database_boundaries_are_exact_and_names_are_literal() {
        for prefix in ["", "prod", "prod/"] {
            let namespace = StorageNamespace::new(prefix).unwrap();
            let database = namespace.database(DatabaseName::new("notes").unwrap());
            let neighbor = namespace.database(DatabaseName::new("notes_old").unwrap());
            assert!(database.relative_key(&neighbor.head_key()).is_err());
            assert!(database.relative_key(database.prefix()).is_err());
            assert!(database
                .relative_key(&format!("{}../other", database.prefix()))
                .is_err());
            for name in [
                "中文", "a b", "Notes", "notes", "a%b", "%2F", "é", "e\u{301}",
            ] {
                let name = DatabaseName::new(name).unwrap();
                let path = namespace.database(name.clone());
                assert_eq!(namespace.database_name(path.prefix()).unwrap(), name);
            }
        }
        let namespace = StorageNamespace::default();
        for bad in [
            "other/notes/",
            "databases/",
            "databases/notes",
            "databases/notes/child/",
            "databases/notes//",
        ] {
            assert!(namespace.database_name(bad).is_err(), "{bad}");
        }
        assert!(StorageNamespace::new("prod//").is_err());
    }

    #[test]
    fn relative_key_safety_and_layout_parsing_are_distinct() {
        for id in ["", ".", "..", "a/b", "a\\b", "%2e", "中文"] {
            assert!(IncarnationId::new(id).is_err(), "{id}");
        }
        for key in [
            "head.json",
            "incarnations/inc/segments/old-layout",
            "incarnations/inc/generations/0000000000000000/deltas/x",
            "incarnations/inc/generations/000000000000000A/deltas/x",
            "incarnations/inc/generations/000000000000001/deltas/x",
            "incarnations/inc/generations/not-an-epoch/deltas/x",
            "incarnations/inc/generations/0000000000000001/deltas",
        ] {
            assert!(
                RelativeKey::new(key).unwrap().object_parts().is_none(),
                "{key}"
            );
        }
        // Nested safe suffixes remain accepted, as in the existing v1 parser.
        let key =
            RelativeKey::new("incarnations/inc/generations/0000000000000001/deltas/segments/x")
                .unwrap();
        assert_eq!(key.object_parts().unwrap().category, "deltas");
        assert_eq!(key.object_parts().unwrap().object, "segments/x");
        assert!(object_key(
            &IncarnationId::new("inc").unwrap(),
            0,
            &RelativeKey::new("deltas").unwrap(),
            "x"
        )
        .is_err());
    }
}
