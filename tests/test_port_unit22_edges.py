"""Focused cases for middleware / dependency-injection synthetic edges."""

from __future__ import annotations

from pathlib import Path

from coderadar.framework import refresh_framework_file


def _run(tmp_path: Path, files: dict[str, str], rel: str) -> dict:
    for path, text in files.items():
        target = tmp_path / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    return refresh_framework_file(str(tmp_path), rel, files.get(rel))


def test_fastapi_middleware_and_di(tmp_path):
    report = _run(
        tmp_path,
        {
            "main.py": """\
from fastapi import FastAPI, Depends
app = FastAPI()

def get_db():
    return None

@app.middleware("http")
async def add_headers(request, call_next):
    return await call_next(request)

app.add_middleware(SecurityMiddleware)

@app.get('/items')
def list_items(db=Depends(get_db)):
    return []
""",
        },
        "main.py",
    )
    assert report["handlers"] >= 2


def test_express_middleware(tmp_path):
    report = _run(
        tmp_path,
        {
            "package.json": '{"dependencies": {"express": "^4.18.0"}}\n',
            "app.js": """\
const express = require('express');
const app = express();
app.use(logger);
app.use('/api', authMiddleware, router);
app.get('/users', listUsers);
""",
        },
        "app.js",
    )
    assert report["handlers"] >= 1


def test_rails_callbacks(tmp_path):
    report = _run(
        tmp_path,
        {
            "Gemfile": "gem 'rails', '~> 7.0'\n",
            "app/controllers/users_controller.rb": """\
class UsersController < ApplicationController
  before_action :authenticate_user, only: [:edit]
  after_action :log_request
  def edit; end
end
""",
        },
        "app/controllers/users_controller.rb",
    )
    assert report["routes"] >= 2


def test_nestjs_di(tmp_path):
    report = _run(
        tmp_path,
        {
            "package.json": '{"dependencies": {"@nestjs/core": "^10.0.0"}}\n',
            "app.module.ts": """\
@Injectable()
export class UserService {}

@Module({
  providers: [UserService],
  controllers: [UserController],
})
export class AppModule {}
""",
        },
        "app.module.ts",
    )
    assert report["handlers"] >= 2


def test_flask_hooks(tmp_path):
    report = _run(
        tmp_path,
        {
            "app.py": """\
from flask import Flask
app = Flask(__name__)

@app.before_request
def load_user():
    return None

@app.errorhandler(404)
def not_found(error):
    return 'missing', 404

@app.route('/')
def index():
    return 'ok'
""",
        },
        "app.py",
    )
    assert report["handlers"] >= 1


def test_aspnet_middleware(tmp_path):
    report = _run(
        tmp_path,
        {
            "MyApi.csproj": '<Project Sdk="Microsoft.NET.Sdk.Web"></Project>\n',
            "Program.cs": """\
[ApiController]
var builder = WebApplication.CreateBuilder(args);
var app = builder.Build();
app.UseMiddleware<RequestLoggingMiddleware>();
app.UseAuthorization();
app.MapGet("/users", ListUsers);
""",
        },
        "Program.cs",
    )
    assert report["handlers"] >= 1


def test_go_middleware(tmp_path):
    report = _run(
        tmp_path,
        {
            "go.mod": "module example\n",
            "server.go": """\
package main

import "github.com/gin-gonic/gin"

func main() {
	r := gin.Default()
	r.Use(authMiddleware)
	r.GET("/users", listUsers)
}
""",
        },
        "server.go",
    )
    assert report["handlers"] >= 2


def test_spring_di(tmp_path):
    report = _run(
        tmp_path,
        {
            "pom.xml": "<dependency><groupId>org.springframework.boot</groupId></dependency>\n",
            "UserController.java": """\
@RestController
public class UserController {
    @Autowired
    private UserService userService;

    @Bean
    public CacheManager cacheManager() { return null; }

    @GetMapping("/users")
    public List<User> list() { return null; }
}
""",
        },
        "UserController.java",
    )
    assert report["handlers"] >= 1
