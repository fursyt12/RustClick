//! Mapping from logical buttons to logical actions.
//!
//! Mappings are keyed by [`ButtonId`] only, never by device model, so the
//! same configuration applies to every remote that reports a given button.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::input::ButtonId;

/// A logical action the target application should perform.
///
/// Serialized as a short string: `next`, `blank_screen`, `custom:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Action {
    Next,
    Previous,
    StartPresentation,
    StopPresentation,
    BlankScreen,
    Pointer,
    Custom(String),
}

/// The fixed actions, in display order. `Custom` is handled separately.
pub const BUILTIN_ACTIONS: [Action; 6] = [
    Action::Next,
    Action::Previous,
    Action::StartPresentation,
    Action::StopPresentation,
    Action::BlankScreen,
    Action::Pointer,
];

impl Action {
    pub fn label(&self) -> String {
        match self {
            Self::Next => "Next".into(),
            Self::Previous => "Previous".into(),
            Self::StartPresentation => "Start presentation".into(),
            Self::StopPresentation => "Stop presentation".into(),
            Self::BlankScreen => "Blank screen".into(),
            Self::Pointer => "Pointer".into(),
            Self::Custom(name) => format!("Custom: {name}"),
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Next => "next".into(),
            Self::Previous => "previous".into(),
            Self::StartPresentation => "start_presentation".into(),
            Self::StopPresentation => "stop_presentation".into(),
            Self::BlankScreen => "blank_screen".into(),
            Self::Pointer => "pointer".into(),
            Self::Custom(name) => format!("custom:{name}"),
        }
    }

    /// Builds a custom action, validating its name.
    pub fn custom(name: &str) -> Result<Self, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 48 {
            return Err("custom action name must be 1 to 48 characters".into());
        }
        Ok(Self::Custom(name.to_owned()))
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

impl TryFrom<String> for Action {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if let Some(name) = value.strip_prefix("custom:") {
            return Self::custom(name);
        }
        BUILTIN_ACTIONS
            .iter()
            .find(|a| a.key() == value)
            .cloned()
            .ok_or_else(|| format!("unknown action `{value}`"))
    }
}

impl From<Action> for String {
    fn from(value: Action) -> Self {
        value.key()
    }
}

/// Button → action table. Buttons without an entry are unmapped and their
/// events are rejected by the router.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Mappings(BTreeMap<ButtonId, Action>);

impl Default for Mappings {
    fn default() -> Self {
        let pairs = [
            ("forward", Action::Next),
            ("back", Action::Previous),
            ("forward_hold", Action::StartPresentation),
            ("back_hold", Action::StopPresentation),
            ("top", Action::BlankScreen),
            ("top_hold", Action::Pointer),
        ];
        Self(
            pairs
                .into_iter()
                .map(|(b, a)| (ButtonId::standard(b), a))
                .collect(),
        )
    }
}

impl Mappings {
    pub fn empty() -> Self {
        Self(BTreeMap::new())
    }

    pub fn resolve(&self, button: &ButtonId) -> Option<&Action> {
        self.0.get(button)
    }

    /// Sets or clears (`None`) the mapping for `button`.
    pub fn set(&mut self, button: ButtonId, action: Option<Action>) {
        match action {
            Some(action) => {
                self.0.insert(button, action);
            }
            None => {
                self.0.remove(&button);
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ButtonId, &Action)> {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mapping_covers_navigation() {
        let m = Mappings::default();
        assert_eq!(
            m.resolve(&ButtonId::standard("forward")),
            Some(&Action::Next)
        );
        assert_eq!(
            m.resolve(&ButtonId::standard("back")),
            Some(&Action::Previous)
        );
        assert_eq!(m.resolve(&ButtonId::standard("aux")), None);
    }

    #[test]
    fn set_and_clear() {
        let mut m = Mappings::default();
        let aux = ButtonId::standard("aux");
        m.set(aux.clone(), Some(Action::custom("toggle notes").unwrap()));
        assert_eq!(
            m.resolve(&aux),
            Some(&Action::Custom("toggle notes".into()))
        );
        m.set(aux.clone(), None);
        assert_eq!(m.resolve(&aux), None);
    }

    #[test]
    fn actions_round_trip_as_strings() {
        for action in BUILTIN_ACTIONS
            .iter()
            .cloned()
            .chain([Action::Custom("x y".into())])
        {
            let s: String = action.clone().into();
            assert_eq!(Action::try_from(s).unwrap(), action);
        }
        assert!(Action::try_from("custom:".to_string()).is_err());
        assert!(Action::try_from("teleport".to_string()).is_err());
    }

    #[test]
    fn mappings_serialize_as_a_toml_table() {
        #[derive(Serialize, Deserialize)]
        struct W {
            mappings: Mappings,
        }
        let text = toml::to_string(&W {
            mappings: Mappings::default(),
        })
        .unwrap();
        assert!(text.contains("forward = \"next\""), "{text}");
        let back: W = toml::from_str(&text).unwrap();
        assert_eq!(back.mappings, Mappings::default());
    }
}
