use std::cmp::Ordering;

pub fn vercmp(a: &str, b: &str) -> Ordering {
    let (a_epoch, a_rest) = split_epoch(a);
    let (b_epoch, b_rest) = split_epoch(b);

    match a_epoch.cmp(&b_epoch) {
        Ordering::Equal => {}
        other => return other,
    }

    let (a_ver, a_rel) = split_rel(a_rest);
    let (b_ver, b_rel) = split_rel(b_rest);

    match vercmp_part(a_ver, b_ver) {
        Ordering::Equal => vercmp_part(a_rel, b_rel),
        other => other,
    }
}

pub fn vercmp_dep(have: &str, want: &str) -> Ordering {
    let (have_epoch, have_rest) = split_epoch(have);
    let (want_epoch, want_rest) = split_epoch(want);

    if have_epoch != want_epoch {
        return have_epoch.cmp(&want_epoch);
    }

    let (have_ver, have_rel) = split_rel(have_rest);
    let (want_ver, want_rel) = split_rel(want_rest);

    match vercmp_part(have_ver, want_ver) {
        Ordering::Equal => {}
        other => return other,
    }

    if want_rel.is_empty() {
        return Ordering::Equal;
    }

    vercmp_part(have_rel, want_rel)
}

fn split_epoch(v: &str) -> (u64, &str) {
    match v.split_once(':') {
        Some((e, rest)) => match e.parse::<u64>() {
            Ok(n) => (n, rest),
            Err(_) => (0, v),
        },
        None => (0, v),
    }
}

fn split_rel(v: &str) -> (&str, &str) {
    match v.rfind('-') {
        Some(i) => (&v[..i], &v[i + 1..]),
        None => (v, ""),
    }
}

fn vercmp_part(a: &str, b: &str) -> Ordering {
    let mut ai = a.split(|c: char| !c.is_alphanumeric());
    let mut bi = b.split(|c: char| !c.is_alphanumeric());

    loop {
        let x = ai.next().unwrap_or("");
        let y = bi.next().unwrap_or("");
        if x.is_empty() && y.is_empty() {
            return Ordering::Equal;
        }

        if x.is_empty() {
            return Ordering::Less;
        }
        if y.is_empty() {
            return Ordering::Greater;
        }

        let xn = x.parse::<u64>();
        let yn = y.parse::<u64>();
        let ord = match (xn, yn) {
            (Ok(a), Ok(b)) => a.cmp(&b),

            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn equal_versions_compare_equal() {
        assert_eq!(vercmp("1.0.0-1", "1.0.0-1"), Equal);
        assert_eq!(vercmp("1:1.0-1", "1:1.0-1"), Equal);
    }

    #[test]
    fn numeric_segments_compare_numerically() {
        assert_eq!(vercmp("1.10.0-1", "1.9.0-1"), Greater);
        assert_eq!(vercmp("1.2.3-1", "1.2.10-1"), Less);
    }

    #[test]
    fn the_package_release_breaks_ties() {
        assert_eq!(vercmp("1.0.0-2", "1.0.0-1"), Greater);
        assert_eq!(vercmp("1.0.0-10", "1.0.0-9"), Greater);
    }

    #[test]
    fn the_epoch_wins_over_everything() {
        assert_eq!(vercmp("1:1.0-1", "2.0-1"), Greater);
        assert_eq!(vercmp("1:1.0-1", "1.0-1"), Greater);

        assert_eq!(vercmp("0:9.9-1", "1.0-1"), Greater);
    }

    #[test]
    fn numeric_segments_beat_alphabetic_ones() {
        assert_eq!(vercmp("1.0.1-1", "1.0.a-1"), Greater);
        assert_eq!(vercmp("1.0.a-1", "1.0.1-1"), Less);
    }

    #[test]
    fn a_missing_release_is_older() {
        assert_eq!(vercmp("1.0.0", "1.0.0-1"), Less);
    }

    #[test]
    fn a_dependency_without_a_release_matches_any_release() {
        assert_eq!(vercmp_dep("0.196-1", "0.196"), Equal);
        assert_eq!(vercmp_dep("0.196-2", "0.196"), Equal);
        assert_eq!(vercmp_dep("1.2.5.1-4", "1.2.5.1"), Equal);
        assert_eq!(vercmp_dep("51.0-2", "51.0"), Equal);
    }

    #[test]
    fn a_dependency_with_a_release_must_match_it() {
        assert_eq!(vercmp_dep("0.196-1", "0.196-1"), Equal);
        assert_ne!(vercmp_dep("0.196-1", "0.196-2"), Equal);
        assert_ne!(vercmp_dep("1.2.5.1-4", "1.2.5"), Equal);
    }

    #[test]
    fn the_version_part_must_match_exactly() {
        assert_ne!(vercmp_dep("1.2.5.1-4", "1.2.5"), Equal);
        assert_ne!(vercmp_dep("1.2.5.0-4", "1.2.5.1"), Equal);
    }
}
