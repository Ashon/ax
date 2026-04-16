package tasknote

import "errors"

type Task struct {
	ID    int    `json:"id"`
	Title string `json:"title"`
	Done  bool   `json:"done"`
}

func AddTask(tasks []Task, title string) ([]Task, Task, error) {
	return nil, Task{}, errors.New("not implemented")
}

func CompleteTask(tasks []Task, id int) ([]Task, error) {
	return nil, errors.New("not implemented")
}

func RenderMarkdown(tasks []Task) string {
	return ""
}
