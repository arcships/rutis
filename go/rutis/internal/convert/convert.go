// Package convert moves Go values across the session: Go values to wire
// values (peer values) and back into typed Go values, and calls Go
// functions and methods with wire arguments.
//
// Data (numbers, strings, slices, maps, structs) is copied as JSON;
// functions, Ref(v) and the far end's references cross by reference.
package convert

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"reflect"
	"strings"
	"sync"
	"unicode"

	"github.com/arcships/rutis/go/rutis/internal/peer"
)

var (
	ctxType      = reflect.TypeFor[context.Context]()
	errType      = reflect.TypeFor[error]()
	anyType      = reflect.TypeFor[any]()
	futureType   = reflect.TypeFor[*Future]()
	functionType = reflect.TypeFor[*Function]()
)

// ── Names ────────────────────────────────────────────────────────

// WireName is the name a Go method or field has on the wire: the first
// letter lowercased; a leading run of capitals lowercased as a whole,
// except that when a lowercase letter follows the run, its last capital
// starts the next word (URLFor -> urlFor, HTTPServer -> httpServer).
func WireName(name string) string {
	runes := []rune(name)
	n := 0
	for n < len(runes) && unicode.IsUpper(runes[n]) {
		n++
	}
	if n == 0 {
		return name
	}
	lower := n
	if n > 1 && n < len(runes) && unicode.IsLower(runes[n]) {
		lower = n - 1
	}
	for i := 0; i < lower; i++ {
		runes[i] = unicode.ToLower(runes[i])
	}
	return string(runes)
}

// ── References ───────────────────────────────────────────────────

type ref struct{ value any }

// Ref marks v to cross by reference: the far end gets a proxy calling v's
// exported methods.
func Ref(v any) any { return ref{v} }

// Function is a function of the far end, received where a value of any
// type was expected.
type Function struct{ remote *peer.RemoteFunction }

// Call calls the function and decodes its result into `result` (a
// pointer, or nil).
func (f *Function) Call(ctx context.Context, result any, args ...any) error {
	converted, err := ToPeerArgs(args)
	if err != nil {
		return err
	}
	value, err := f.remote.Call(ctx, converted, true)
	if err != nil {
		return err
	}
	return Decode(value, result)
}

// Future is an asynchronous result of the far end.
type Future struct{ remote *peer.RemoteFuture }

// Await waits for the result and decodes it into `result` (a pointer, or
// nil); ctx cancels the wait.
func (f *Future) Await(ctx context.Context, result any) error {
	value, err := f.remote.Await(ctx)
	if err != nil {
		return err
	}
	if value, err = peer.Settle(ctx, value); err != nil {
		return err
	}
	return Decode(value, result)
}

// ── Go -> wire ───────────────────────────────────────────────────

// ToPeerArgs converts arguments.
func ToPeerArgs(args []any) ([]any, error) {
	out := make([]any, len(args))
	for i, arg := range args {
		converted, err := ToPeer(arg)
		if err != nil {
			return nil, fmt.Errorf("argument %d: %w", i, err)
		}
		out[i] = converted
	}
	return out, nil
}

// ToPeer converts a Go value to a peer value.
func ToPeer(v any) (any, error) {
	switch x := v.(type) {
	case nil:
		return nil, nil
	case peer.UndefinedType, json.RawMessage, *peer.RemoteFunction, *peer.RemoteFuture, peer.Func, peer.Object:
		return x, nil
	case ref:
		if x.value == nil {
			return nil, nil
		}
		return Object(x.value), nil
	case *Future:
		if x == nil {
			return nil, nil
		}
		return x.remote, nil
	case *Function:
		if x == nil {
			return nil, nil
		}
		return x.remote, nil
	case context.Context:
		return nil, errors.New("a context.Context cannot cross between processes")
	case error:
		return x, nil
	}
	return toPeerValue(reflect.ValueOf(v))
}

func toPeerValue(rv reflect.Value) (any, error) {
	switch rv.Kind() {
	case reflect.Invalid:
		return nil, nil
	case reflect.Func:
		if rv.IsNil() {
			return nil, nil
		}
		return peer.Func(func(ctx context.Context, args []any) (any, error) {
			return Call(ctx, rv, args)
		}), nil
	case reflect.Interface:
		if rv.IsNil() {
			return nil, nil
		}
		return ToPeer(rv.Elem().Interface())
	case reflect.Pointer:
		if rv.IsNil() {
			return nil, nil
		}
	case reflect.Chan, reflect.Complex64, reflect.Complex128, reflect.UnsafePointer:
		return nil, fmt.Errorf("%s cannot cross between processes", rv.Type())
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		if n := rv.Int(); n > peer.MaxSafe || n < -peer.MaxSafe {
			return nil, fmt.Errorf("the integer %d is beyond ±2^53-1, which crosses between processes intact", n)
		}
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		if n := rv.Uint(); n > peer.MaxSafe {
			return nil, fmt.Errorf("the integer %d is beyond 2^53-1, which crosses between processes intact", n)
		}
	case reflect.Slice, reflect.Array:
		if rv.Type().Elem().Kind() == reflect.Uint8 {
			return nil, fmt.Errorf("%s cannot cross between processes: encode bytes as a string", rv.Type())
		}
		if rv.Kind() == reflect.Slice && rv.IsNil() {
			return nil, nil
		}
		if mayHoldReference(rv.Type().Elem(), 0) {
			items := make([]any, rv.Len())
			for i := range items {
				item, err := toPeerValue(rv.Index(i))
				if err != nil {
					return nil, err
				}
				items[i] = item
			}
			return items, nil
		}
	case reflect.Map:
		if rv.Type().Key().Kind() != reflect.String {
			return nil, fmt.Errorf("%s cannot cross between processes: only string keys do", rv.Type())
		}
		if rv.IsNil() {
			return nil, nil
		}
		if mayHoldReference(rv.Type().Elem(), 0) {
			items := make(map[string]any, rv.Len())
			iter := rv.MapRange()
			for iter.Next() {
				item, err := toPeerValue(iter.Value())
				if err != nil {
					return nil, err
				}
				items[iter.Key().String()] = item
			}
			return items, nil
		}
	}
	data, err := json.Marshal(rv.Interface())
	if err != nil {
		return nil, fmt.Errorf("%s cannot cross between processes: %w", rv.Type(), err)
	}
	return json.RawMessage(data), nil
}

func mayHoldReference(t reflect.Type, depth int) bool {
	if depth > 8 {
		return false
	}
	switch t.Kind() {
	case reflect.Func, reflect.Interface:
		return true
	case reflect.Slice, reflect.Array, reflect.Map:
		return mayHoldReference(t.Elem(), depth+1)
	case reflect.Pointer:
		return t == futureType || t == functionType
	}
	return false
}

// ── wire -> Go ───────────────────────────────────────────────────

// Decode decodes a peer value into `out`, a pointer (nil: discard).
func Decode(v any, out any) error {
	if out == nil {
		return nil
	}
	target := reflect.ValueOf(out)
	if target.Kind() != reflect.Pointer || target.IsNil() {
		return errors.New("a result must be decoded into a non-nil pointer")
	}
	value, err := FromPeer(v, target.Type().Elem())
	if err != nil {
		return err
	}
	target.Elem().Set(value)
	return nil
}

// FromPeer decodes a peer value as a value of type t.
func FromPeer(v any, t reflect.Type) (reflect.Value, error) {
	switch x := v.(type) {
	case nil, peer.UndefinedType, *peer.Signal:
		return reflect.Zero(t), nil
	case json.RawMessage:
		if t == anyType {
			plain, err := Plain(x)
			if err != nil {
				return reflect.Value{}, err
			}
			return valueOf(plain, t), nil
		}
		if t == errType {
			var failure struct{ Name, Message string }
			if err := json.Unmarshal(x, &failure); err != nil {
				return reflect.Value{}, err
			}
			return reflect.ValueOf(error(&peer.RemoteError{Name: failure.Name, Message: failure.Message})), nil
		}
		if string(x) == "null" {
			return reflect.Zero(t), nil
		}
		out := reflect.New(t)
		if err := json.Unmarshal(x, out.Interface()); err != nil {
			return reflect.Value{}, fmt.Errorf("cannot decode %s as %s: %w", abbreviate(x), t, err)
		}
		return out.Elem(), nil
	case *peer.RemoteFunction:
		switch {
		case t == functionType:
			return reflect.ValueOf(&Function{x}), nil
		case t.Kind() == reflect.Func:
			return MakeFunc(t, func(ctx context.Context, args []any) (any, error) {
				return x.Call(ctx, args, true)
			}), nil
		case t == anyType:
			return reflect.ValueOf(any(&Function{x})), nil
		}
		return reflect.Value{}, fmt.Errorf("a function cannot be decoded as %s", t)
	case peer.Func:
		if t.Kind() == reflect.Func {
			return MakeFunc(t, x), nil
		}
		return reflect.Value{}, fmt.Errorf("a function cannot be decoded as %s", t)
	case *peer.RemoteFuture:
		switch t {
		case futureType:
			return reflect.ValueOf(&Future{x}), nil
		case anyType:
			return reflect.ValueOf(any(&Future{x})), nil
		}
		return reflect.Value{}, fmt.Errorf("an asynchronous result cannot be decoded as %s: use *rutis.Future", t)
	case peer.Object:
		if reflect.TypeOf(x).AssignableTo(t) {
			return reflect.ValueOf(x), nil
		}
		if o, ok := x.(object); ok && o.rv.Type().AssignableTo(t) {
			return o.rv, nil
		}
		return reflect.Value{}, fmt.Errorf("an object cannot be decoded as %s", t)
	case []any:
		switch t.Kind() {
		case reflect.Slice, reflect.Array:
			out := reflect.New(t).Elem()
			if t.Kind() == reflect.Slice {
				out = reflect.MakeSlice(t, len(x), len(x))
			} else if len(x) > t.Len() {
				return reflect.Value{}, fmt.Errorf("%d items do not fit %s", len(x), t)
			}
			for i, item := range x {
				value, err := FromPeer(item, t.Elem())
				if err != nil {
					return reflect.Value{}, err
				}
				out.Index(i).Set(value)
			}
			return out, nil
		case reflect.Interface:
			plain, err := Plain(x)
			if err != nil {
				return reflect.Value{}, err
			}
			return valueOf(plain, t), nil
		case reflect.Pointer:
			inner, err := FromPeer(x, t.Elem())
			if err != nil {
				return reflect.Value{}, err
			}
			out := reflect.New(t.Elem())
			out.Elem().Set(inner)
			return out, nil
		}
		return reflect.Value{}, fmt.Errorf("a list cannot be decoded as %s", t)
	case map[string]any:
		switch t.Kind() {
		case reflect.Map:
			if t.Key().Kind() != reflect.String {
				return reflect.Value{}, fmt.Errorf("a record cannot be decoded as %s", t)
			}
			out := reflect.MakeMapWithSize(t, len(x))
			for key, item := range x {
				value, err := FromPeer(item, t.Elem())
				if err != nil {
					return reflect.Value{}, err
				}
				out.SetMapIndex(reflect.ValueOf(key).Convert(t.Key()), value)
			}
			return out, nil
		case reflect.Struct:
			out := reflect.New(t).Elem()
			for key, item := range x {
				field, ok := fieldByJSONName(t, key)
				if !ok {
					continue
				}
				value, err := FromPeer(item, field.Type)
				if err != nil {
					return reflect.Value{}, err
				}
				out.FieldByIndex(field.Index).Set(value)
			}
			return out, nil
		case reflect.Pointer:
			inner, err := FromPeer(x, t.Elem())
			if err != nil {
				return reflect.Value{}, err
			}
			out := reflect.New(t.Elem())
			out.Elem().Set(inner)
			return out, nil
		case reflect.Interface:
			plain, err := Plain(x)
			if err != nil {
				return reflect.Value{}, err
			}
			return valueOf(plain, t), nil
		}
		return reflect.Value{}, fmt.Errorf("a record cannot be decoded as %s", t)
	}
	value := reflect.ValueOf(v)
	if value.Type().AssignableTo(t) {
		return value, nil
	}
	return reflect.Value{}, fmt.Errorf("%T cannot be decoded as %s", v, t)
}

func valueOf(v any, t reflect.Type) reflect.Value {
	if v == nil {
		return reflect.Zero(t)
	}
	return reflect.ValueOf(v)
}

func abbreviate(raw json.RawMessage) string {
	if len(raw) > 60 {
		return string(raw[:57]) + "..."
	}
	return string(raw)
}

// Plain is a peer value as plain Go values: JSON data decoded as
// encoding/json does into `any`, references as *Function and *Future.
func Plain(v any) (any, error) {
	switch x := v.(type) {
	case nil, peer.UndefinedType, *peer.Signal:
		return nil, nil
	case json.RawMessage:
		var out any
		if err := json.Unmarshal(x, &out); err != nil {
			return nil, err
		}
		return out, nil
	case []any:
		out := make([]any, len(x))
		for i, item := range x {
			plain, err := Plain(item)
			if err != nil {
				return nil, err
			}
			out[i] = plain
		}
		return out, nil
	case map[string]any:
		out := make(map[string]any, len(x))
		for key, item := range x {
			plain, err := Plain(item)
			if err != nil {
				return nil, err
			}
			out[key] = plain
		}
		return out, nil
	case *peer.RemoteFunction:
		return &Function{x}, nil
	case *peer.RemoteFuture:
		return &Future{x}, nil
	case object:
		return x.rv.Interface(), nil
	}
	return v, nil
}

func fieldByJSONName(t reflect.Type, name string) (reflect.StructField, bool) {
	var fold *reflect.StructField
	for _, field := range reflect.VisibleFields(t) {
		if !field.IsExported() || field.Anonymous {
			continue
		}
		key := field.Name
		if tag, ok := field.Tag.Lookup("json"); ok {
			tagName, _, _ := strings.Cut(tag, ",")
			if tagName == "-" {
				continue
			}
			if tagName != "" {
				key = tagName
			}
		}
		if key == name {
			return field, true
		}
		if fold == nil && strings.EqualFold(key, name) {
			f := field
			fold = &f
		}
	}
	if fold != nil {
		return *fold, true
	}
	return reflect.StructField{}, false
}

// ── Calls ────────────────────────────────────────────────────────

// Call calls the Go function `fn` with wire arguments: a leading
// context.Context parameter gets ctx; the others are decoded by position
// (missing ones are zero values, extra ones an error); a trailing error
// result is the call's error. A panic becomes a peer.PanicError.
func Call(ctx context.Context, fn reflect.Value, args []any) (any, error) {
	return peer.Protect(func() (any, error) {
		ft := fn.Type()
		for len(args) > 0 {
			if _, undefined := args[len(args)-1].(peer.UndefinedType); !undefined {
				break
			}
			args = args[:len(args)-1]
		}
		in := make([]reflect.Value, 0, ft.NumIn())
		first := 0
		if ft.NumIn() > 0 && ft.In(0) == ctxType {
			in = append(in, reflect.ValueOf(&ctx).Elem())
			first = 1
		}
		fixed := ft.NumIn() - first
		if ft.IsVariadic() {
			fixed--
		}
		if !ft.IsVariadic() && len(args) > fixed {
			return nil, fmt.Errorf("%d arguments given, %d expected", len(args), fixed)
		}
		for i := 0; i < fixed; i++ {
			var arg any = peer.Undefined
			if i < len(args) {
				arg = args[i]
			}
			value, err := FromPeer(arg, ft.In(first+i))
			if err != nil {
				return nil, fmt.Errorf("argument %d: %w", i, err)
			}
			in = append(in, value)
		}
		if ft.IsVariadic() {
			elem := ft.In(ft.NumIn() - 1).Elem()
			for i := fixed; i < len(args); i++ {
				value, err := FromPeer(args[i], elem)
				if err != nil {
					return nil, fmt.Errorf("argument %d: %w", i, err)
				}
				in = append(in, value)
			}
		}
		out := fn.Call(in)
		return Results(out)
	})
}

// Results converts a Go call's results: a trailing error is the error,
// and at most one other result is the value.
func Results(out []reflect.Value) (any, error) {
	if n := len(out); n > 0 && out[n-1].Type() == errType {
		if !out[n-1].IsNil() {
			return nil, out[n-1].Interface().(error)
		}
		out = out[:n-1]
	}
	switch len(out) {
	case 0:
		return peer.Undefined, nil
	case 1:
		return ToPeer(out[0].Interface())
	}
	return nil, fmt.Errorf("a function returns at most one value and an error, not %d values", len(out))
}

// CheckSignature reports whether t can be called across processes: an
// optional leading context.Context, and at most one result before an
// optional trailing error.
func CheckSignature(t reflect.Type, needError bool) error {
	n := t.NumOut()
	hasError := n > 0 && t.Out(n-1) == errType
	if hasError {
		n--
	}
	if n > 1 {
		return fmt.Errorf("%s returns %d values: at most one value and an error cross between processes", t, n)
	}
	if needError && !hasError {
		return fmt.Errorf("%s must return an error last", t)
	}
	return nil
}

// MakeFunc makes a Go function of type t from `call`: a leading
// context.Context argument is its ctx (nil panics), the other arguments
// are converted, and the result is decoded as t's result. Without an
// error result, a failure panics.
func MakeFunc(t reflect.Type, call func(ctx context.Context, args []any) (any, error)) reflect.Value {
	return reflect.MakeFunc(t, func(in []reflect.Value) []reflect.Value {
		ctx := context.Background()
		first := 0
		if t.NumIn() > 0 && t.In(0) == ctxType {
			first = 1
			if in[0].IsNil() {
				panic("rutis: a nil context: pass the context.Context your caller gave you")
			}
			ctx = in[0].Interface().(context.Context)
		}
		var args []any
		for i := first; i < len(in); i++ {
			if t.IsVariadic() && i == len(in)-1 {
				for j := 0; j < in[i].Len(); j++ {
					args = append(args, in[i].Index(j).Interface())
				}
				continue
			}
			args = append(args, in[i].Interface())
		}
		var value any
		converted, err := ToPeerArgs(args)
		if err == nil {
			value, err = call(ctx, converted)
		}
		if err == nil {
			value, err = peer.Settle(ctx, value)
		}
		return results(t, value, err)
	})
}

func results(t reflect.Type, value any, err error) []reflect.Value {
	n := t.NumOut()
	hasError := n > 0 && t.Out(n-1) == errType
	out := make([]reflect.Value, n)
	for i := range out {
		out[i] = reflect.Zero(t.Out(i))
	}
	if err == nil && n > 0 && (!hasError || n == 2) {
		decoded, decodeErr := FromPeer(value, t.Out(0))
		if decodeErr != nil {
			err = decodeErr
		} else {
			out[0] = decoded
		}
	}
	if err != nil {
		if !hasError {
			panic(err)
		}
		out[n-1] = reflect.ValueOf(&err).Elem()
	}
	return out
}

// ── Objects ──────────────────────────────────────────────────────

type object struct{ rv reflect.Value }

// Object is v as an object the far end calls: its exported methods by
// wire name, its exported fields as properties.
func Object(v any) peer.Object { return object{reflect.ValueOf(v)} }

func (o object) Call(ctx context.Context, method string, args []any) (any, error) {
	index, ok := Methods(o.rv.Type())[method]
	if !ok {
		return nil, fmt.Errorf("unknown method %s", method)
	}
	return Call(ctx, o.rv.Method(index), args)
}

func (o object) Get(_ context.Context, property string) (any, error) {
	return Property(o.rv, property)
}

// Property reads the exported field of v named `name` on the wire (its
// json name, or its wire name).
func Property(v reflect.Value, name string) (any, error) {
	for v.Kind() == reflect.Pointer || v.Kind() == reflect.Interface {
		if v.IsNil() {
			return nil, fmt.Errorf("no property %s", name)
		}
		v = v.Elem()
	}
	if v.Kind() == reflect.Struct {
		if field, ok := fieldByJSONName(v.Type(), name); ok {
			return ToPeer(v.FieldByIndex(field.Index).Interface())
		}
		for _, field := range reflect.VisibleFields(v.Type()) {
			if field.IsExported() && WireName(field.Name) == name {
				return ToPeer(v.FieldByIndex(field.Index).Interface())
			}
		}
	}
	return nil, fmt.Errorf("no property %s", name)
}

var methodTables sync.Map

// Methods maps the wire names of t's exported methods to their indexes.
func Methods(t reflect.Type) map[string]int {
	if cached, ok := methodTables.Load(t); ok {
		return cached.(map[string]int)
	}
	table := map[string]int{}
	for i := 0; i < t.NumMethod(); i++ {
		table[WireName(t.Method(i).Name)] = i
	}
	methodTables.Store(t, table)
	return table
}
