package main

import "net/http"

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /users", listUsers)
	mux.HandleFunc("POST /users", createUser)
	mux.Handle("/health", http.HandlerFunc(healthCheck))
}

func listUsers(w http.ResponseWriter, r *http.Request)   {}
func createUser(w http.ResponseWriter, r *http.Request)  {}
func healthCheck(w http.ResponseWriter, r *http.Request) {}
