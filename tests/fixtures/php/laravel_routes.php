<?php

use Illuminate\Support\Facades\Route;
use App\Http\Controllers\UserController;
use App\Http\Controllers\OrderController;

// ── Direct route definitions ──

Route::get('/users', [UserController::class, 'index']);
Route::post('/users', [UserController::class, 'store']);
Route::put('/users/{id}', [UserController::class, 'update']);
Route::delete('/users/{id}', [UserController::class, 'destroy']);
Route::patch('/users/{id}', [UserController::class, 'partialUpdate']);

// Older string syntax
Route::post('/login', 'AuthController@login');

// Any method
Route::any('/health', 'HealthController@check');

// ── Resource routes ──

Route::resource('orders', OrderController::class);

// ── Route group with prefix ──

Route::group(['prefix' => 'admin', 'middleware' => 'auth'], function () {
    Route::get('/dashboard', [AdminController::class, 'dashboard']);
    Route::get('/settings', [AdminController::class, 'settings']);
});

// ── Named route with array handler ──

Route::get('/profile', ['uses' => 'ProfileController@show', 'as' => 'profile']);

// ── Inline closure (no named reference — should be ignored) ──

Route::get('/version', function () {
    return response()->json(['version' => '1.0']);
});
