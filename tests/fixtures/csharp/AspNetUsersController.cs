using Microsoft.AspNetCore.Mvc;
using System.Collections.Generic;

namespace MyApi.Controllers;

[ApiController]
[Route("api/[controller]")]
public class UsersController : ControllerBase
{
    [HttpGet]
    public IActionResult GetUsers()
    {
        return Ok(new List<User>());
    }

    [HttpGet("{id}")]
    public IActionResult GetById(int id)
    {
        return Ok(new User { Id = id });
    }

    [HttpPost]
    public IActionResult CreateUser([FromBody] User user)
    {
        return Created("", user);
    }

    [HttpPut("{id}")]
    public IActionResult UpdateUser(int id, [FromBody] User user)
    {
        return Ok(user);
    }

    [HttpDelete("{id}")]
    public IActionResult DeleteUser(int id)
    {
        return NoContent();
    }

    [HttpPatch("{id}")]
    public IActionResult PartialUpdate(int id, [FromBody] JsonPatchDocument<User> patch)
    {
        return Ok();
    }
}

[ApiController]
[Route("api/products")]
public class ProductsController : ControllerBase
{
    [HttpGet]
    public IActionResult List() => Ok();

    [HttpPost]
    public IActionResult Create([FromBody] Product p) => Created("", p);
}

// Controller without [Route] base path
[ApiController]
public class HealthController : ControllerBase
{
    [HttpGet("/health")]
    public IActionResult Check() => Ok(new { status = "UP" });

    [HttpGet("/version")]
    public IActionResult Version() => Ok(new { version = "1.0.0" });
}
