package com.example.controller;

import org.springframework.web.bind.annotation.*;
import org.springframework.http.ResponseEntity;
import java.util.List;

@RestController
@RequestMapping("/api/users")
public class UserController {

    @GetMapping
    public List<User> listUsers() {
        return userService.findAll();
    }

    @GetMapping("/{id}")
    public User getById(@PathVariable Long id) {
        return userService.findById(id);
    }

    @PostMapping
    public User createUser(@RequestBody User user) {
        return userService.save(user);
    }

    @PutMapping("/{id}")
    public User updateUser(@PathVariable Long id, @RequestBody User user) {
        return userService.update(id, user);
    }

    @DeleteMapping("/{id}")
    public void deleteUser(@PathVariable Long id) {
        userService.delete(id);
    }

    @GetMapping("/search")
    public List<User> searchUsers(@RequestParam String q) {
        return userService.search(q);
    }
}

@RestController
@RequestMapping("/api/orders")
public class OrderController {

    @PostMapping
    public Order createOrder(@RequestBody Order order) {
        return orderService.place(order);
    }

    @GetMapping("/{id}")
    public Order getOrder(@PathVariable Long id) {
        return orderService.findById(id);
    }
}

@RestController
public class HealthController {

    // Simple GET without class-level path prefix
    @GetMapping("/health")
    public ResponseEntity<String> check() {
        return ResponseEntity.ok("UP");
    }

    // @RequestMapping with explicit method
    @RequestMapping(path = "/status", method = RequestMethod.GET)
    public ResponseEntity<String> status() {
        return ResponseEntity.ok("healthy");
    }
}
