import JCodeKit
import SwiftUI

/// Friendly placeholder for a fresh session, centered in the canvas.
struct EmptyTranscript: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "terminal")
                .font(Theme.icon(40, weight: .light))
                .foregroundStyle(Theme.textSecondary)
            Text("Ready when you are")
                .font(Theme.mono(16, weight: .medium))
                .foregroundStyle(Theme.textPrimary)
            Text("Send a message to start driving this session.")
                .font(.subheadline)
                .foregroundStyle(Theme.textSecondary)
                .multilineTextAlignment(.center)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// One transcript entry: user bubble, assistant markdown, or system note.
struct EntryView: View {
    let entry: TranscriptEntry

    var body: some View {
        switch entry.role {
        case .user:
            HStack {
                Spacer(minLength: 48)
                VStack(alignment: .trailing, spacing: 4) {
                    Text(entry.text)
                        .font(.body)
                        .foregroundStyle(Theme.textPrimary)
                        .padding(12)
                        .background(Theme.mintTint)
                        .clipShape(RoundedRectangle(cornerRadius: 16))
                        .copyContextMenu(entry.text)
                    if entry.isQueued {
                        Label("queued", systemImage: "clock")
                            .font(Theme.mono(11))
                            .foregroundStyle(Theme.textTertiary)
                            .accessibilityLabel("Queued")
                            .accessibilityHint("Delivers after the current response")
                    }
                }
            }
        case .assistant:
            VStack(alignment: .leading, spacing: 8) {
                if !entry.reasoning.isEmpty {
                    Text(entry.reasoning)
                        .font(Theme.mono(12))
                        .italic()
                        .foregroundStyle(Theme.textTertiary)
                        .lineLimit(4)
                        .copyContextMenu(entry.reasoning)
                }
                ForEach(entry.toolCalls) { call in
                    ToolCallCard(call: call)
                }
                if !entry.text.isEmpty {
                    MarkdownText(entry.text)
                        .copyContextMenu(entry.text)
                }
            }
        case .system:
            Text(entry.text)
                .font(.footnote)
                .foregroundStyle(Theme.textTertiary)
                .frame(maxWidth: .infinity, alignment: .center)
                .copyContextMenu(entry.text)
        }
    }
}

extension View {
    /// Long-press context menu offering to copy the given text.
    func copyContextMenu(_ text: String) -> some View {
        contextMenu {
            Button {
                UIPasteboard.general.string = text
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
            }
        }
    }
}
