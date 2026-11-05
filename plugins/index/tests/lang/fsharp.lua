local th = require("maki.test_helpers")
local helpers = require("tests.helpers")
local case = th.case
local idx = helpers.idx
local has = helpers.has

case("fsharp_all_sections", function()
  local src = [==[
namespace MyApp.Core

open System
open System.Collections.Generic

let MaxRetries = 3

let add (x: int) (y: int) : int =
    x + y

type Status =
    | Active = 0
    | Inactive = 1

type Shape =
    | Circle of radius: float
    | Rectangle of width: float * height: float

type Point = {
    X: float
    Y: float
}

type ICalculator =
    abstract member Add: int -> int -> int
    abstract member Reset: unit -> unit

type UserService(name: string) =
    member _.Name = name
    member _.Process(input: string) : bool = true
    static member Default() = UserService("default")

module Helper =
    let format (s: string) = s.Trim()
]==]
  local out = idx(src, "fsharp")
  has(out, {
    "imports:",
    "System",
    "Collections.Generic",
    "mod:",
    "MyApp.Core",
    "Helper",
    "classes:",
    "type UserService(name: string)",
    "member _.Name",
    "member _.Process(input: string) : bool",
    "static member Default()",
    "traits:",
    "type ICalculator",
    "abstract member Add: int -> int -> int",
    "abstract member Reset: unit -> unit",
    "fns:",
    "let add (x: int) (y: int) : int",
    "let format (s: string)",
    "consts:",
    "let MaxRetries",
    "types:",
    "type Status",
    "Active",
    "Inactive",
    "type Shape",
    "Circle",
    "Rectangle",
    "type Point",
    "X: float",
    "Y: float",
  })
end)

case("fsharp_mutually_recursive_types", function()
  local src = [==[
namespace MyApp

type Node(value: int) =
    member _.Value = value
    member _.Next : Edge option = None
and Edge(target: Node) =
    member _.Target = target
]==]
  local out = idx(src, "fsharp")
  has(out, {
    "classes:",
    "type Node(value: int)",
    "member _.Value",
    "type Edge(target: Node)",
    "member _.Target",
  })
end)

case("fsharp_static_policy_and_enums", function()
  local src = [==[
namespace CoreLib

/// 持久化值不可重排；定时只拥有整次启动。
type ScheduleStatus =
    | Armed = 0
    | StartCommitted = 1
    | Finished = 2
    | Cancelled = 3
    | Expired = 4
    | Interrupted = 5

[<AbstractClass; Sealed>]
type LocalSchedulePolicy private () =
    static member CanStart(status: ScheduleStatus, dueUtcMs: int64, deadlineUtcMs: int64, nowUtcMs: int64) =
        status = ScheduleStatus.Armed && nowUtcMs >= dueUtcMs && nowUtcMs < deadlineUtcMs

    static member IsExpired(status: ScheduleStatus, deadlineUtcMs: int64, nowUtcMs: int64) =
        status = ScheduleStatus.Armed && nowUtcMs >= deadlineUtcMs

    static member CanEdit(status: ScheduleStatus) = status = ScheduleStatus.Armed

    static member Recover(status: ScheduleStatus) =
        if status = ScheduleStatus.StartCommitted then ScheduleStatus.Interrupted else status
]==]
  local out = idx(src, "fsharp")
  has(out, {
    "mod:",
    "CoreLib",
    "types:",
    "type ScheduleStatus",
    "Armed",
    "StartCommitted",
    "Finished",
    "Cancelled",
    "Expired",
    "Interrupted",
    "classes:",
    "type LocalSchedulePolicy private ()",
    "static member CanStart(status: ScheduleStatus, dueUtcMs: int64, deadlineUtcMs: int64, nowUtcMs: int64)",
    "static member IsExpired(status: ScheduleStatus, deadlineUtcMs: int64, nowUtcMs: int64)",
    "static member CanEdit(status: ScheduleStatus)",
    "static member Recover(status: ScheduleStatus)",
  })
end)

case("fsharp_class_constructors_and_properties", function()
  local src = [==[
namespace Batching

open System
open System.Collections.Generic
open System.Text.RegularExpressions
open Batching.Template

[<Sealed>]
type BatchValidationIssue(rowIndex: int, columnIndex: int, message: string, blocksSending: bool) =
    member _.RowIndex = rowIndex
    member _.ColumnIndex = columnIndex
    member _.Message = message
    member _.BlocksSending = blocksSending

type BatchRowStatus =
    | Passed = 0
    | Warning = 1
    | Skipped = 2
    | Error = 3
]==]
  local out = idx(src, "fsharp")
  has(out, {
    "imports:",
    "System",
    "Collections.Generic",
    "Text.RegularExpressions",
    "Batching.Template",
    "mod:",
    "Batching",
    "classes:",
    "type BatchValidationIssue(rowIndex: int, columnIndex: int, message: string, blocksSending: bool)",
    "member _.RowIndex",
    "member _.ColumnIndex",
    "member _.Message",
    "member _.BlocksSending",
    "types:",
    "type BatchRowStatus",
    "Passed",
    "Warning",
    "Skipped",
    "Error",
  })
end)

case("fsharp_record_with_extensions_and_interface", function()
  local src = [==[
namespace Logging

open System

type LogLevel =
    | Verbose = 0
    | Info = 1
    | Error = 2

type LogEntry() =
    member val Timestamp : DateTime = DateTime.MinValue with get, set
    member val Level : LogLevel = LogLevel.Verbose with get, set
    member val Message : string = null with get, set
    member this.DisplayMessage = this.Message

type StateInputs =
    {
        mutable SearchKeyword : string
        mutable SelectFirstSlot : bool
    }
    static member None : StateInputs = Unchecked.defaultof<StateInputs>
    member this.HasKeyword = not (String.IsNullOrEmpty(this.SearchKeyword))
]==]
  local out = idx(src, "fsharp")
  has(out, {
    "mod:",
    "Logging",
    "types:",
    "type LogLevel",
    "Verbose",
    "Info",
    "Error",
    "type StateInputs",
    "SearchKeyword : string",
    "SelectFirstSlot : bool",
    "classes:",
    "type LogEntry()",
    "member val Timestamp : DateTime",
    "member val Level : LogLevel",
    "member val Message : string",
    "member this.DisplayMessage",
  })
end)

case("fsharp_signature_file", function()
  local src = [==[
namespace CoreLib.Data

module ColumnNames =
    val ToColumnName: colIndex: int -> string
    val TryParseColumnIndex: name: string -> int option
]==]
  local out = idx(src, "fsharp_signature")
  has(out, {
    "mod:",
    "CoreLib.Data",
    "ColumnNames",
    "fns:",
    "val ToColumnName: colIndex: int -> string",
    "val TryParseColumnIndex: name: string -> int option",
  })
end)

case("fsharp_tests_detection", function()
  local src = [==[
namespace CoreLib.Tests

open System

[<Test>]
let testScheduleCalculation () =
    Assert.True(true)

[<Fact>]
let factCheckSomething () =
    ()

let normalFunction x = x + 1
]==]
  local out = idx(src, "fsharp")
  has(out, {
    "tests:",
    "fns:",
    "let normalFunction x",
  })
  helpers.lacks(out, {
    "let testScheduleCalculation",
    "let factCheckSomething",
  })
end)
