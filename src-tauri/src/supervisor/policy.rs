use std::time::Duration;

const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(25),
];

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Restart { after: Duration },
    GiveUp,
}

pub fn decide(restarts_so_far: usize) -> Decision {
    match BACKOFF.get(restarts_so_far) {
        Some(after) => Decision::Restart { after: *after },
        None => Decision::GiveUp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_back_off_and_stop_after_the_third() {
        let seconds: Vec<_> = (0..4)
            .map(|done| match decide(done) {
                Decision::Restart { after } => Some(after.as_secs()),
                Decision::GiveUp => None,
            })
            .collect();
        assert_eq!(seconds, [Some(1), Some(5), Some(25), None]);
    }
}
