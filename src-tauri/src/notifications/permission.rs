//! Whether macOS lets Humla post notifications. The permission belongs to the
//! app bundle, so it is read and requested in this process.

use objc2_user_notifications::UNAuthorizationStatus;

/// macOS's answer to whether Humla may post notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Authorization {
    Granted,
    Denied,
    NotDetermined,
    Unknown,
}

impl Authorization {
    /// In the vocabulary of `PermissionStatus` (src/lib/ipc.ts).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Authorization::Granted => "granted",
            Authorization::Denied => "denied",
            Authorization::NotDetermined => "not_determined",
            Authorization::Unknown => "unknown",
        }
    }
}

impl From<UNAuthorizationStatus> for Authorization {
    fn from(status: UNAuthorizationStatus) -> Self {
        match status {
            UNAuthorizationStatus::NotDetermined => Authorization::NotDetermined,
            UNAuthorizationStatus::Denied => Authorization::Denied,
            UNAuthorizationStatus::Authorized
            | UNAuthorizationStatus::Provisional
            | UNAuthorizationStatus::Ephemeral => Authorization::Granted,
            _ => Authorization::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(status: UNAuthorizationStatus) -> &'static str {
        Authorization::from(status).label()
    }

    #[test]
    fn macos_answers_map_to_the_permission_vocabulary() {
        assert_eq!(label(UNAuthorizationStatus::NotDetermined), "not_determined");
        assert_eq!(label(UNAuthorizationStatus::Denied), "denied");
        assert_eq!(label(UNAuthorizationStatus::Authorized), "granted");
    }

    #[test]
    fn every_kind_of_yes_reads_as_granted() {
        assert_eq!(label(UNAuthorizationStatus::Provisional), "granted");
        assert_eq!(label(UNAuthorizationStatus::Ephemeral), "granted");
    }

    #[test]
    fn an_answer_this_build_doesnt_know_reads_as_unknown() {
        assert_eq!(label(UNAuthorizationStatus(99)), "unknown");
    }
}
