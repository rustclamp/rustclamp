use rustclamp::prelude::*;

fn main() {
    Clamp::run(|| rustclamp::web::serve(::__CRATE__::routes()));
}
