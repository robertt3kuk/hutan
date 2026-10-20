package main

import (
	"fmt"
	"strings"
)

// Server serves things.
type Server struct {
	Name string
	port int
}

const Limit = 10

func (s *Server) Start(args []string) error {
	if len(args) > Limit {
		return fmt.Errorf("too many: %d", len(args))
	}
	for i, a := range args {
		fmt.Println(i, strings.ToUpper(a), 'x', 3.14)
	}
	return nil
}

func main() {
	s := &Server{Name: "x", port: 80}
	_ = s.Start(nil)
}
