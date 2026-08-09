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

func listUsers(c *gin.Context)    {}
func createUser(c *gin.Context)   {}
func getUser(c *gin.Context)      {}
func updateUser(c *gin.Context)   {}
func deleteUser(c *gin.Context)   {}
func listItems(c *gin.Context)    {}
