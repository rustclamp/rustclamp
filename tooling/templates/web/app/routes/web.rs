use rustclamp::web::Router;

use crate::http::controllers::home;
use crate::http::kernel;

/// Pages. The welcome page needs no session, so it stays outside the `web`
/// group; routes with forms go inside it.
pub fn routes(router: Router) -> Router {
    router.get("/", home::index).group(|web| {
        kernel::web(web)
        // .get("/contact", contact::show).post("/contact", contact::store)
    })
}
