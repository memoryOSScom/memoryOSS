---
description: Search your long-term memory for relevant context
argument-hint: [query or topic to search for]
---

# Recall Memories

Search memoryOSS for relevant memories matching the user's query:

1. Take the query from the arguments (or ask for one if not provided)
2. Call the `memoryoss_recall` MCP tool with the query
3. Present the results clearly, showing:
   - Memory content
   - Relevance score
   - When it was stored
   - Tags if any
4. If no results found, let the user know their memory is empty for this topic
5. Suggest storing new information if relevant context is missing
