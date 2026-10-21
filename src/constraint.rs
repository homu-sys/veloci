use crate::version::vercmp_dep;
use std::cmp::Ordering;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Any,
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
pub struct Constraint {
    pub name: String,
    pub op: Op,
    pub version: Option<String>,
}

impl Constraint {
    pub fn parse(s: &str) -> Constraint {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'<' || c == b'>' || c == b'=' {
                let two = i + 1 < bytes.len() && bytes[i + 1] == b'=';
                let (op, len) = match (c, two) {
                    (b'<', true) => (Op::Le, 2),
                    (b'>', true) => (Op::Ge, 2),
                    (b'<', false) => (Op::Lt, 1),
                    (b'>', false) => (Op::Gt, 1),
                    (_, true) => (Op::Eq, 2),
                    (_, false) => (Op::Eq, 1),
                };
                let name = s[..i].trim().to_string();
                let version = s[i + len..].trim().to_string();
                return Constraint {
                    name,
                    op,
                    version: if version.is_empty() {
                        None
                    } else {
                        Some(version)
                    },
                };
            }
            i += 1;
        }
        Constraint {
            name: s.trim().to_string(),
            op: Op::Any,
            version: None,
        }
    }

    pub fn matches_version(&self, version: &str) -> bool {
        match self.op {
            Op::Any => true,
            _ => {
                let Some(want) = &self.version else {
                    return true;
                };
                matches(vercmp_dep(version, want), self.op)
            }
        }
    }

    pub fn matches_provided(&self, provided: Option<&str>) -> bool {
        match self.op {
            Op::Any => true,
            _ => {
                let (Some(have), Some(want)) = (provided, &self.version) else {
                    return false;
                };
                matches(vercmp_dep(have, want), self.op)
            }
        }
    }
}

fn matches(ord: Ordering, op: Op) -> bool {
    match op {
        Op::Any => true,
        Op::Eq => ord == Ordering::Equal,
        Op::Lt => ord == Ordering::Less,
        Op::Le => ord != Ordering::Greater,
        Op::Gt => ord == Ordering::Greater,
        Op::Ge => ord != Ordering::Less,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_has_no_version_rule() {
        let c = Constraint::parse("glibc");
        assert_eq!(c.name, "glibc");
        assert_eq!(c.op, Op::Any);
        assert!(c.version.is_none());
    }

    #[test]
    fn operators_are_parsed() {
        assert_eq!(Constraint::parse("foo=1.0").op, Op::Eq);
        assert_eq!(Constraint::parse("foo>=1.0").op, Op::Ge);
        assert_eq!(Constraint::parse("foo<=1.0").op, Op::Le);
        assert_eq!(Constraint::parse("foo<1.0").op, Op::Lt);
        assert_eq!(Constraint::parse("foo>1.0").op, Op::Gt);
        assert_eq!(Constraint::parse("foo>=1.0").name, "foo");
        assert_eq!(
            Constraint::parse("foo>=1.0").version.as_deref(),
            Some("1.0")
        );
    }

    #[test]
    fn a_versioned_dependency_needs_a_matching_provider() {
        let c = Constraint::parse("libverto.so=0-64");
        assert!(c.matches_provided(Some("0-64")));
        assert!(!c.matches_provided(Some("0-32")));
        assert!(!c.matches_provided(None));
    }

    #[test]
    fn an_unversioned_dependency_takes_any_provider() {
        let c = Constraint::parse("libverto.so");
        assert!(c.matches_provided(Some("0-64")));
        assert!(c.matches_provided(None));
    }

    #[test]
    fn ranges_are_honoured() {
        let c = Constraint::parse("foo>=1.0");
        assert!(c.matches_version("1.0"));
        assert!(c.matches_version("2.0"));
        assert!(!c.matches_version("0.9"));
    }
}
