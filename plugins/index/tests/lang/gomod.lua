local th = require("maki.test_helpers")
local helpers = require("tests.helpers")
local case = th.case
local idx = helpers.idx
local idx_with_meta = helpers.idx_with_meta
local has = helpers.has
local lacks = helpers.lacks

case("gomod_module_go_toolchain", function()
  local src = [[
module example.com/my/project

go 1.22.0

toolchain go1.22.2
]]
  local out = idx(src, "gomod")
  has(out, {
    "module:",
    "  example.com/my/project [1]",
    "go:",
    "  1.22.0 [3]",
    "toolchain:",
    "  go1.22.2 [5]",
  })
end)

case("gomod_quoted_module", function()
  local src = 'module "example.com/quoted/module"\n\ngo 1.21\n'
  local out = idx(src, "gomod")
  has(out, {
    "module:",
    "  example.com/quoted/module [1]",
    "go:",
    "  1.21 [3]",
  })
end)

case("gomod_require_single_and_multi", function()
  local src = [[
module example.com/app

go 1.23.0

require github.com/stretchr/testify v1.8.4

require (
    github.com/gin-gonic/gin v1.9.1
    golang.org/x/sync v0.6.0 // indirect
)
]]
  local out = idx(src, "gomod")
  has(out, {
    "require:",
    "  github.com/stretchr/testify v1.8.4 [5]",
    "  github.com/gin-gonic/gin v1.9.1 [8]",
    "  golang.org/x/sync v0.6.0 // indirect [9]",
  })
end)

case("gomod_replace_and_exclude", function()
  local src = [[
module example.com/service

go 1.22

replace (
    example.com/foo => ../foo
    example.com/bar v1.0.0 => example.com/baz v1.1.0
)

exclude example.com/bad v1.0.0
]]
  local out = idx(src, "gomod")
  has(out, {
    "replace:",
    "  example.com/foo => ../foo [6]",
    "  example.com/bar v1.0.0 => example.com/baz v1.1.0 [7]",
    "exclude:",
    "  example.com/bad v1.0.0 [10]",
  })
end)

case("gomod_retract_and_tool_and_godebug", function()
  local src = [[
module example.com/lib

go 1.24

retract (
    v1.0.0 // broken API
    [v1.0.1, v1.0.2]
)

tool golang.org/x/tools/cmd/stringer

godebug default=go1.21
]]
  local out = idx(src, "gomod")
  has(out, {
    "retract:",
    "  v1.0.0 // broken API [6]",
    "  [v1.0.1, v1.0.2] [7]",
    "tool:",
    "  golang.org/x/tools/cmd/stringer [10]",
    "godebug:",
    "  default=go1.21 [12]",
  })
end)

case("gomod_ranged_meta", function()
  local src = [[
module example.com/meta

go 1.22.0

require (
    github.com/foo/bar v1.0.0
    github.com/baz/qux v2.0.0 // indirect
)
]]
  local out, meta = idx_with_meta(src, "gomod")
  helpers.assert_ranged_meta(out, meta, {
    "example.com/meta",
    "1.22.0",
    "github.com/foo/bar v1.0.0",
    "github.com/baz/qux v2.0.0 // indirect",
  })
end)

case("gomod_comments_and_empty", function()
  local src = "// A comment only\n// Another comment\n"
  local out = idx(src, "gomod")
  assert(out == "", "expected empty output, got: " .. out)
end)

case("gomod_filename_and_ext_mapping", function()
  local indexer = require("indexer")
  local eq = th.eq
  eq(indexer.FILENAME_TO_LANG["go.mod"], "gomod")
  eq(indexer.EXT_TO_LANG["mod"], "gomod")
end)

case("gomod_syntax_error", function()
  local src = "require (\n"
  local indexer = require("indexer")
  local out = idx(src, "gomod")
  assert(out == indexer.PARSE_ERROR_NOTE .. "\n", "expected parse error note, got: " .. out)
end)
