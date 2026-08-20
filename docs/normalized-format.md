# Normalized conversation format

Collectors can import one JSON object, a JSON array, or newline-delimited JSON. The stable identity is `(source, source_instance, source_conversation_id)`.

```json
{
  "source": "gemini",
  "source_instance": null,
  "source_conversation_id": "provider-conversation-id",
  "title": "Conversation title",
  "create_time": 1787184000.0,
  "update_time": 1787187600.0,
  "model": "gemini-model-name",
  "source_url": "https://gemini.google.com/app/example",
  "source_path": null,
  "messages": [
    {
      "message_id": "provider-message-id",
      "role": "user",
      "create_time": 1787184000.0,
      "text": "Message text",
      "raw": {}
    }
  ],
  "raw": {}
}
```

Use Unix timestamps in seconds. `source_instance` is optional and should distinguish accounts only when provider IDs are not globally unique. Collectors must produce stable message IDs and retain provider-specific data under `raw`.
