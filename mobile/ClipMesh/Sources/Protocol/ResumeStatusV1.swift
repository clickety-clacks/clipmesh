enum ResumeStatusV1: String, Equatable {
    case complete
    case epochChanged = "epoch_changed"
    case fresh
    case gap
    case generationChanged = "generation_changed"
}
