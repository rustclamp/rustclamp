//! Infrastructure-free tests for shared Users operations and input mappings.

use futures_executor::block_on;
use rustclamp_example_users::{
    ConsoleCommand, MemoryUserRepository, OperationContext, UserId, Users, UsersErrorKind,
    parse_console,
};
use std::time::{Duration, Instant};

fn context() -> OperationContext {
    OperationContext::new(
        Some("direct-test".into()),
        "tenant-a",
        "direct-1",
        Instant::now() + Duration::from_secs(1),
        || false,
    )
}

#[test]
fn direct_and_console_paths_call_the_same_user_operations() {
    block_on(async {
        let repository = MemoryUserRepository::default();
        let probe = repository.probe();
        let mut users = Users::new(repository);
        let created = users.create(&context(), "  Ada  ").await.unwrap();
        assert_eq!(created.name, "Ada");
        assert_eq!(users.get(&context(), created.id).await.unwrap(), created);
        assert_eq!(probe.calls(), 2);

        let command = parse_console(&["get".into(), created.id.0.to_string()]).unwrap();
        assert_eq!(command, ConsoleCommand::Get { id: created.id });
        assert!(parse_console(&["get".into(), "bad".into()]).is_err());
        assert_eq!(
            users.get(&context(), UserId(999)).await.unwrap_err().kind(),
            UsersErrorKind::NotFound
        );
    });
}

#[test]
fn authorization_validation_tenant_and_deadline_are_domain_rules() {
    block_on(async {
        let mut users = Users::new(MemoryUserRepository::default());
        let anonymous = OperationContext::new(
            None,
            "tenant-a",
            "anonymous",
            Instant::now() + Duration::from_secs(1),
            || false,
        );
        assert_eq!(
            users.create(&anonymous, "Ada").await.unwrap_err().kind(),
            UsersErrorKind::Unauthorized
        );
        assert_eq!(
            users.create(&context(), "  ").await.unwrap_err().kind(),
            UsersErrorKind::InvalidName
        );
        let expired = OperationContext::new(
            Some("direct".into()),
            "tenant-a",
            "expired",
            Instant::now(),
            || false,
        );
        assert_eq!(
            users.get(&expired, UserId(1)).await.unwrap_err().kind(),
            UsersErrorKind::DeadlineExceeded
        );
        let cancelled = OperationContext::new(
            Some("direct".into()),
            "tenant-a",
            "cancelled",
            Instant::now() + Duration::from_secs(1),
            || true,
        );
        assert_eq!(
            users.get(&cancelled, UserId(1)).await.unwrap_err().kind(),
            UsersErrorKind::Cancelled
        );
    });
}
