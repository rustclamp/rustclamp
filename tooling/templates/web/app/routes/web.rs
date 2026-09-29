use rustclamp::web::Router;

/// Pages. Put routes that need sessions in a group with `Sessions` and `csrf()`.
pub fn routes(router: Router) -> Router {
    router.view("/", "welcome")
}
