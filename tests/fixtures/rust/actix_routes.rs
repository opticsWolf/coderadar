use actix_web::{get, post, put, delete, web, App, HttpServer, HttpResponse};

#[get("/users")]
async fn list_users() -> HttpResponse {
    HttpResponse::Ok().finish()
}

#[post("/users")]
async fn create_user() -> HttpResponse {
    HttpResponse::Ok().finish()
}

#[get("/users/{id}")]
async fn get_user(path: web::Path<u32>) -> HttpResponse {
    HttpResponse::Ok().finish()
}

#[put("/users/{id}")]
async fn update_user(path: web::Path<u32>) -> HttpResponse {
    HttpResponse::Ok().finish()
}

#[delete("/users/{id}")]
async fn delete_user(path: web::Path<u32>) -> HttpResponse {
    HttpResponse::Ok().finish()
}

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health_check))
       .route("/metrics", web::get().to(metrics));
}

async fn health_check() -> HttpResponse {
    HttpResponse::Ok().finish()
}

async fn metrics() -> HttpResponse {
    HttpResponse::Ok().finish()
}
