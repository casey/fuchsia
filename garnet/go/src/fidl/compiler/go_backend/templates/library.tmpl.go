// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package templates

const Library = `
{{- define "GenerateLibraryFile" -}}
// Code generated by fidlgen; DO NOT EDIT.

package {{ .Name }}

{{if .Libraries}}
import (
{{- range .Libraries }}
	{{ .Alias }} "{{ .Path }}"
{{- end }}
)
{{end}}

{{if .Consts}}
const (
{{- range $const := .Consts }}
	{{- range .DocComments}}
	//{{ . }}
	{{- end}}
	{{ .Name }} {{ .Type }} = {{ .Value }}
{{- end }}
)
{{end}}

{{ range $enum := .Enums -}}
{{ template "EnumDefinition" $enum }}
{{ end -}}
{{ range $bits := .Bits -}}
{{ template "BitsDefinition" $bits }}
{{ end -}}
{{ range $struct := .Structs -}}
{{ template "StructDefinition" $struct }}
{{ end -}}
{{ range $xunion := .XUnions -}}
{{ template "XUnionDefinition" $xunion }}
{{ end -}}
{{ range $table := .Tables -}}
{{ template "TableDefinition" $table }}
{{ end -}}
{{ range $interface := .Interfaces -}}
{{ template "InterfaceDefinition" $interface }}
{{ end -}}

{{- end -}}
`