use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};
use std::{
    fs::Metadata,
    os::unix::fs::MetadataExt,
    time::{SystemTime, UNIX_EPOCH},
};

macro_rules! id_types {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub i64);

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                i64::column_result(value).map(Self)
            }
        }
    )+};
}

id_types!(FolderId, PhotoId, VariantId, KeywordId, CollectionId, SnapshotId, PresetId);

/// Maps an enum to the small integer that represents it in the database.
macro_rules! coded_enum {
    ($name:ident { $($variant:ident = $code:expr),+ $(,)? }) => {
        impl $name {
            pub(crate) fn code(self) -> i64 {
                match self { $(Self::$variant => $code),+ }
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.code()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                match i64::column_result(value)? {
                    $($code => Ok(Self::$variant),)+
                    other => Err(FromSqlError::OutOfRange(other)),
                }
            }
        }
    };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    #[default]
    Unflagged,
    Pick,
    Reject,
}

coded_enum!(Flag { Unflagged = 0, Pick = 1, Reject = -1 });

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    #[default]
    None,
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

coded_enum!(Label { None = 0, Red = 1, Yellow = 2, Green = 3, Blue = 4, Purple = 5 });

impl Label {
    /// The text `xmp:Label` uses.
    pub(crate) fn name(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Red => Some("Red"),
            Self::Yellow => Some("Yellow"),
            Self::Green => Some("Green"),
            Self::Blue => Some("Blue"),
            Self::Purple => Some("Purple"),
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        [Self::Red, Self::Yellow, Self::Green, Self::Blue, Self::Purple]
            .into_iter()
            .find(|label| label.name().is_some_and(|known| known.eq_ignore_ascii_case(name)))
    }
}

/// Whether the catalog can currently use a photo's file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PhotoStatus {
    #[default]
    Available,
    Missing,
    /// LibRaw can't open it. Kept so a rescan doesn't retry the file until it changes.
    Unsupported,
}

coded_enum!(PhotoStatus { Available = 0, Missing = 1, Unsupported = 2 });

/// Identifies a file's content from its size and its first and last 64 KiB. Hashing whole
/// raws would cost hundreds of gigabytes of reads on a large library, and a raw's header and
/// tail are distinct between shots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 16]);

impl Fingerprint {
    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl ToSql for Fingerprint {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.0.as_slice()))
    }
}

impl FromSql for Fingerprint {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let bytes = value.as_blob()?;
        bytes.try_into().map(Self).map_err(|_| FromSqlError::InvalidBlobSize {
            expected_size: 16,
            blob_size: bytes.len(),
        })
    }
}

/// Size and modification time, which together decide whether a file needs reading again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub size: i64,
    pub mtime_ns: i64,
}

impl FileStamp {
    pub fn of(metadata: &Metadata) -> Self {
        Self {
            size: metadata.size() as i64,
            mtime_ns: metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}
