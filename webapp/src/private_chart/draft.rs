//! Browser-owned original inputs; deliberately neither Debug nor serializable as a view.

#[derive(Clone, PartialEq)]
pub(super) struct Draft {
    pub precision: String,
    pub date: String,
    pub time: String,
    pub zone: String,
    pub offset_seconds: String,
    pub uncertainty_seconds: String,
    pub longitude: String,
    pub latitude: String,
    pub bodies: Vec<String>,
    pub houses: String,
    pub consent: bool,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            precision: "exact".into(),
            date: String::new(),
            time: String::new(),
            zone: String::new(),
            offset_seconds: String::new(),
            uncertainty_seconds: String::new(),
            longitude: String::new(),
            latitude: String::new(),
            bodies: ephemeris::lines::LINE_BODIES
                .into_iter()
                .map(|body| body.name().to_owned())
                .collect(),
            houses: "none".into(),
            consent: false,
        }
    }
}

impl Draft {
    pub fn payload(&self) -> Result<serde_json::Value, &'static str> {
        use chrono::{Datelike, Timelike};
        use serde_json::json;
        const INVALID: &str =
            "Check the birth date, clock, time zone, explicit offset and location.";
        if !self.consent {
            return Err("Explicit consent for personal reflection is required.");
        }
        let date = self
            .date
            .parse::<chrono::NaiveDate>()
            .map_err(|_| INVALID)?;
        if !(1900..=2049).contains(&date.year())
            || self.zone.trim().is_empty()
            || self.zone.len() > 64
        {
            return Err(INVALID);
        }
        let longitude = self.longitude.parse::<f64>().map_err(|_| INVALID)?;
        let latitude = self.latitude.parse::<f64>().map_err(|_| INVALID)?;
        if !longitude.is_finite()
            || !(-180.0..=180.0).contains(&longitude)
            || !latitude.is_finite()
            || !(-90.0..=90.0).contains(&latitude)
        {
            return Err(INVALID);
        }
        if !matches!(self.houses.as_str(), "none" | "equal" | "whole_sign")
            || self.bodies.len() > ephemeris::lines::LINE_BODIES.len()
            || self.bodies.iter().enumerate().any(|(index, name)| {
                self.bodies[..index].contains(name)
                    || !ephemeris::lines::LINE_BODIES
                        .iter()
                        .any(|body| body.name() == name)
            })
        {
            return Err("Choose only supported bodies and house conventions.");
        }
        let birth_time = match self.precision.as_str() {
            "unknown" => json!({"precision":"unknown","date":date,"zone":self.zone}),
            "exact" | "approximate" => {
                let time = chrono::NaiveTime::parse_from_str(&self.time, "%H:%M:%S%.f")
                    .or_else(|_| chrono::NaiveTime::parse_from_str(&self.time, "%H:%M"))
                    .map_err(|_| INVALID)?;
                if time.nanosecond() >= 1_000_000_000 {
                    return Err(INVALID);
                }
                let offset = if self.offset_seconds.trim().is_empty() {
                    None
                } else {
                    let value = self.offset_seconds.parse::<i32>().map_err(|_| INVALID)?;
                    if !(-86_400..=86_400).contains(&value) {
                        return Err(INVALID);
                    }
                    Some(value)
                };
                let mut birth = json!({"precision":self.precision,"local":date.and_time(time),"zone":self.zone,"offset_seconds":offset});
                if self.precision == "approximate" {
                    let uncertainty = self
                        .uncertainty_seconds
                        .parse::<u32>()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or(INVALID)?;
                    birth["uncertainty_seconds"] = json!(uncertainty);
                }
                birth
            }
            _ => return Err(INVALID),
        };
        Ok(
            json!({"consent":true,"purpose":"personal_reflection","birth_time":birth_time,
            "longitude_deg":longitude,"latitude_deg":latitude,"bodies":self.bodies,
            "houses":if self.precision == "exact" { self.houses.as_str() } else { "none" }}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Draft {
        Draft {
            date: "2000-01-01".into(),
            time: "12:00".into(),
            zone: "UTC".into(),
            longitude: "0".into(),
            latitude: "0".into(),
            consent: true,
            ..Draft::default()
        }
    }

    #[test]
    fn unknown_clock_has_only_date_and_zone_without_an_invented_instant() {
        let draft = Draft {
            precision: "unknown".into(),
            time: "invalid unused clock".into(),
            offset_seconds: "invalid unused offset".into(),
            houses: "whole_sign".into(),
            ..input()
        };
        let value = draft
            .payload()
            .expect("unknown clock remains a date and zone");
        assert_eq!(
            value["birth_time"],
            serde_json::json!({"precision":"unknown","date":"2000-01-01","zone":"UTC"})
        );
        assert_eq!(value["houses"], "none");
    }

    #[test]
    fn exact_and_approximate_payloads_preserve_explicit_clock_offset_and_uncertainty() {
        let mut draft = input();
        draft.time = "12:00:00.123456789".into();
        draft.offset_seconds = "0".into();
        draft.houses = "equal".into();
        let exact = draft.payload().unwrap();
        assert_eq!(
            exact["birth_time"]["local"],
            "2000-01-01T12:00:00.123456789"
        );
        assert_eq!(exact["birth_time"]["offset_seconds"], 0);
        assert_eq!(exact["houses"], "equal");
        draft.precision = "approximate".into();
        draft.uncertainty_seconds = "300".into();
        let approximate = draft.payload().unwrap();
        assert_eq!(approximate["birth_time"]["uncertainty_seconds"], 300);
        assert_eq!(approximate["houses"], "none");
        assert_eq!(approximate["purpose"], "personal_reflection");
    }

    #[test]
    fn consent_and_invalid_or_unsupported_selections_fail_closed_with_static_errors() {
        let private_marker = "synthetic-private-input";
        for draft in [
            Draft {
                consent: false,
                ..input()
            },
            Draft {
                date: private_marker.into(),
                ..input()
            },
            Draft {
                precision: "unsupported".into(),
                ..input()
            },
            Draft {
                longitude: "NaN".into(),
                ..input()
            },
            Draft {
                houses: "placidus".into(),
                ..input()
            },
            Draft {
                bodies: vec!["true_node".into()],
                ..input()
            },
            Draft {
                precision: "approximate".into(),
                uncertainty_seconds: "0".into(),
                ..input()
            },
        ] {
            let Err(message) = draft.payload() else {
                panic!("invalid input must be refused");
            };
            assert!(!message.contains(private_marker));
        }
    }
}
