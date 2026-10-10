package rutis

import (
	"encoding/json"
	"reflect"
	"testing"
)

type nested struct {
	Port int `json:"port"`
}

type sample struct {
	City    string             `json:"city,omitempty" doc:"the city"`
	Days    int                `json:"days"`
	Tags    []string           `json:"tags,omitempty"`
	Limits  map[string]float64 `json:"limits,omitempty"`
	Server  nested             `json:"server"`
	Skipped string             `json:"-"`
	Plain   bool
}

func TestSchemaFromAStruct(t *testing.T) {
	schema, err := schemaOf(reflect.TypeFor[sample]())
	if err != nil {
		t.Fatal(err)
	}
	got, _ := json.Marshal(schema)
	want := `{"properties":{"Plain":{"type":"boolean"},"city":{"description":"the city","type":"string"},"days":{"type":"integer"},"limits":{"additionalProperties":{"type":"number"},"type":"object"},"server":{"properties":{"port":{"type":"integer"}},"required":["port"],"type":"object"},"tags":{"items":{"type":"string"},"type":"array"}},"required":["days","server","Plain"],"type":"object"}`
	if string(got) != want {
		t.Fatalf("got  %s\nwant %s", got, want)
	}
}

type custom struct{}

func (custom) JSONSchema() map[string]any {
	return map[string]any{"type": "string", "enum": []string{"a"}}
}

func TestASchemerGivesItsOwnSchema(t *testing.T) {
	schema, err := schemaOf(reflect.TypeFor[custom]())
	if err != nil || schema["type"] != "string" {
		t.Fatalf("%v %v", schema, err)
	}
}
