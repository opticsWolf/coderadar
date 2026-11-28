"""Focused cases for Laravel, Spring Boot and ASP.NET route resolvers."""

from __future__ import annotations

from pathlib import Path

from tests._rust_bridge import refresh_framework_file


def _run(tmp_path: Path, files: dict[str, str], rel: str) -> dict:
    for path, text in files.items():
        target = tmp_path / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    return refresh_framework_file(str(tmp_path), rel, files.get(rel))


def test_laravel_crud_and_resource(tmp_path):
    report = _run(
        tmp_path,
        {
            "composer.json": '{"require": {"laravel/framework": "^10.0"}}\n',
            "web.php": """\
<?php
Route::get('/users', [UserController::class, 'index']);
Route::post('/users', [UserController::class, 'store']);
Route::put('/users/{id}', [UserController::class, 'update']);
Route::delete('/users/{id}', [UserController::class, 'destroy']);
Route::resource('products', ProductController::class);
""",
        },
        "web.php",
    )
    assert report["routes"] >= 4


def test_springboot_mappings(tmp_path):
    report = _run(
        tmp_path,
        {
            "pom.xml": "<dependency><groupId>org.springframework.boot</groupId></dependency>\n",
            "UserController.java": """\
@RestController
@RequestMapping("/api/users")
public class UserController {
    @GetMapping
    public List<User> listUsers() { return null; }
    @PostMapping
    public User createUser(@RequestBody User u) { return null; }
    @PutMapping("/{id}")
    public User updateUser(@PathVariable Long id) { return null; }
    @DeleteMapping("/{id}")
    public void deleteUser(@PathVariable Long id) { }
}
""",
        },
        "UserController.java",
    )
    assert report["routes"] >= 4


def test_aspnet_attributes(tmp_path):
    report = _run(
        tmp_path,
        {
            "MyApi.csproj": '<Project Sdk="Microsoft.NET.Sdk.Web"></Project>\n',
            "UsersController.cs": """\
[ApiController]
[Route("api/users")]
public class UsersController : ControllerBase
{
    [HttpGet]
    public IActionResult List() => Ok();
    [HttpGet("{id}")]
    public IActionResult GetById(int id) => Ok();
    [HttpPost]
    public IActionResult Create([FromBody] User u) => Created("", u);
}
""",
        },
        "UsersController.cs",
    )
    assert report["routes"] >= 3
