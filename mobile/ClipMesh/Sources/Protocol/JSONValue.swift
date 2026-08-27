enum JSONValue: Equatable {
    case array([JSONValue])
    case bool(Bool)
    case null
    case number(String)
    case object([String: JSONValue])
    case string(String)
}
