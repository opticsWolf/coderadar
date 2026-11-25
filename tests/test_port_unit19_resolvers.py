"""Focused cases for NestJS, Rails, React/Vue Router, Go and Actix."""

from __future__ import annotations

from pathlib import Path

from coderadar.framework import refresh_framework_file


def _run(tmp_path: Path, files: dict[str, str], rel: str) -> dict:
    for path, text in files.items():
        target = tmp_path / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    return refresh_framework_file(str(tmp_path), rel, files.get(rel))


def test_nestjs_controller(tmp_path):
    report = _run(
        tmp_path,
        {
            "package.json": '{"dependencies": {"@nestjs/core": "^10.0.0"}}\n',
            "user.controller.ts": """\
@Controller('users')
export class UserController {
  @Get()
  async findAll() {}

  @Get(':id')
  async findOne(@Param('id') id: string) {}

  @Post()
  async create(@Body() dto: any) {}
}
""",
        },
        "user.controller.ts",
    )
    assert report["routes"] >= 3


def test_rails_model_associations(tmp_path):
    report = _run(
        tmp_path,
        {
            "Gemfile": "gem 'rails', '~> 7.0'\n",
            "user.rb": """\
class User < ApplicationRecord
  has_many :posts, dependent: :destroy
  belongs_to :organization
  has_one :profile
end
""",
        },
        "user.rb",
    )
    assert report["routes"] >= 3


def test_react_router_jsx(tmp_path):
    report = _run(
        tmp_path,
        {
            "package.json": '{"dependencies": {"react-router-dom": "^6.0.0"}}\n',
            "App.jsx": """\
<Routes>
  <Route path="/" element={<AppLayout />}>
    <Route index element={<HomePage />} />
    <Route path="users" element={<UserList />} />
    <Route path="users/:id" element={<UserDetail />} />
  </Route>
</Routes>
""",
        },
        "App.jsx",
    )
    assert report["routes"] >= 4


def test_vue_router_objects(tmp_path):
    report = _run(
        tmp_path,
        {
            "package.json": '{"dependencies": {"vue-router": "^4.0.0"}}\n',
            "router.js": """\
import { createRouter } from 'vue-router';

const routes = [
  { path: '/', name: 'home', component: HomeView },
  { path: '/users', component: UserList },
  { path: '/users/:id', component: UserDetail },
];

const router = createRouter({
  history: createWebHistory(),
  routes,
});
""",
        },
        "router.js",
    )
    assert report["routes"] >= 3


def test_go_gin_routes(tmp_path):
    report = _run(
        tmp_path,
        {
            "go.mod": "module example\n",
            "gin_routes.go": """\
package main

import "github.com/gin-gonic/gin"

func main() {
	r := gin.Default()
	r.GET("/users", listUsers)
	r.POST("/users", createUser)
	r.GET("/users/:id", getUser)
	r.PUT("/users/:id", updateUser)
	r.DELETE("/users/:id", deleteUser)
	v1 := r.Group("/api/v1")
	v1.GET("/items", listItems)
}
""",
        },
        "gin_routes.go",
    )
    assert report["routes"] >= 6


def test_actix_routes(tmp_path):
    report = _run(
        tmp_path,
        {
            "Cargo.toml": '[dependencies]\nactix-web = "4"\n',
            "actix_routes.rs": """\
use actix_web::{get, post, put, delete, web, App, HttpServer, HttpResponse};

#[get("/users")]
async fn list_users() -> HttpResponse { HttpResponse::Ok().finish() }

#[post("/users")]
async fn create_user() -> HttpResponse { HttpResponse::Ok().finish() }

#[get("/users/{id}")]
async fn get_user(path: web::Path<u32>) -> HttpResponse { HttpResponse::Ok().finish() }

#[put("/users/{id}")]
async fn update_user(path: web::Path<u32>) -> HttpResponse { HttpResponse::Ok().finish() }

#[delete("/users/{id}")]
async fn delete_user(path: web::Path<u32>) -> HttpResponse { HttpResponse::Ok().finish() }

pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health_check))
       .route("/metrics", web::get().to(metrics));
}
""",
        },
        "actix_routes.rs",
    )
    assert report["routes"] >= 7
