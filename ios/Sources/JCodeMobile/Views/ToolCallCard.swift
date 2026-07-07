import JCodeKit
import SwiftUI

/// Collapsible tool call card with live status.
struct ToolCallCard: View {
    let call: TranscriptEntry.ToolCall
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.easeInOut(duration: 0.15)) {
                    expanded.toggle()
                }
            } label: {
                HStack(spacing: 8) {
                    statusIcon
                    Text(call.name)
                        .font(Theme.mono(13, weight: .medium))
                        .foregroundStyle(Theme.textPrimary)
                    Spacer()
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.caption2)
                        .foregroundStyle(Theme.textTertiary)
                }
            }
            if expanded {
                if !call.input.isEmpty {
                    codeBlock(call.input)
                }
                if !call.output.isEmpty {
                    codeBlock(String(call.output.prefix(2000)))
                }
                if case let .failed(message) = call.status {
                    Text(message)
                        .font(Theme.mono(12))
                        .foregroundStyle(Theme.error)
                }
            }
        }
        .padding(8)
        .background(Theme.surfaceElevated)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    @ViewBuilder
    private var statusIcon: some View {
        switch call.status {
        case .streamingInput, .running:
            ProgressView()
                .controlSize(.mini)
                .tint(Theme.mint)
        case .succeeded:
            Image(systemName: "checkmark.circle.fill")
                .font(.caption)
                .foregroundStyle(Theme.mint)
        case .failed:
            Image(systemName: "xmark.circle.fill")
                .font(.caption)
                .foregroundStyle(Theme.error)
        }
    }

    private func codeBlock(_ text: String) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text)
                .font(Theme.mono(11))
                .foregroundStyle(Theme.textSecondary)
                .padding(8)
        }
        .background(Theme.background)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}
