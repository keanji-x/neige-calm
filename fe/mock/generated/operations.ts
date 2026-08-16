// 由 tools/mock/generate.mjs 根据 core/api/generated/openapi.json 与 core/api/generated/wire.ts 生成，禁止手改。
export const mockOperations = [
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "card_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{card_id}/terminal",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Terminal"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "card_id"
      },
      {
        "kind": "literal",
        "value": "/terminal"
      }
    ]
  },
  {
    "method": "DELETE",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}",
    "responses": [
      {
        "bodies": [],
        "status": "204"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "PATCH",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Card"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}/claude/restart",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Card"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/claude/restart"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "query",
        "name": "after_id",
        "required": false,
        "schema": {
          "format": "int64",
          "type": [
            "integer",
            "null"
          ]
        }
      },
      {
        "in": "query",
        "name": "limit",
        "required": false,
        "schema": {
          "format": "int64",
          "type": [
            "integer",
            "null"
          ]
        }
      },
      {
        "in": "query",
        "name": "direction",
        "required": false,
        "schema": {
          "$ref": "#/components/schemas/HarnessItemsDirection"
        }
      }
    ],
    "path": "/api/cards/{id}/harness/items",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/HarnessItem"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/harness/items"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}/ratify",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/RatifyCardResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/ratify"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}/spec/input",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/SendSpecInputResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "503"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/spec/input"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}/spec/interrupt",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/InterruptSpecCardResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/spec/interrupt"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}/spec/reset",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ResetSpecCardResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/spec/reset"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/cards/{id}/spec/run",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/GetSpecRunResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/cards/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/spec/run"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "include_system",
        "required": false,
        "schema": {
          "type": "boolean"
        }
      }
    ],
    "path": "/api/coves",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/Cove"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [],
    "path": "/api/coves",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Cove"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "path",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/resolve",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "oneOf": [
                {
                  "type": "null"
                },
                {
                  "$ref": "#/components/schemas/CoveResolve"
                }
              ]
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/resolve"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [],
    "path": "/api/coves/system",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Cove"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Cove"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/system"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "cove_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{cove_id}/chat-wave/ensure",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Wave"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Wave"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "cove_id"
      },
      {
        "kind": "literal",
        "value": "/chat-wave/ensure"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "cove_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{cove_id}/conversations",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/CoveConversationSummary"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "cove_id"
      },
      {
        "kind": "literal",
        "value": "/conversations"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "cove_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "header",
        "name": "Idempotency-Key",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{cove_id}/conversations",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/CoveConversationSummary"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "503"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "cove_id"
      },
      {
        "kind": "literal",
        "value": "/conversations"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "cove_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{cove_id}/folders",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/CoveFolder"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "cove_id"
      },
      {
        "kind": "literal",
        "value": "/folders"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "cove_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{cove_id}/folders",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/CoveFolder"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/FolderConflict"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "cove_id"
      },
      {
        "kind": "literal",
        "value": "/folders"
      }
    ]
  },
  {
    "method": "DELETE",
    "parameters": [
      {
        "in": "path",
        "name": "cove_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "path",
        "name": "folder_id",
        "required": true,
        "schema": {
          "format": "int64",
          "type": "integer"
        }
      }
    ],
    "path": "/api/coves/{cove_id}/folders/{folder_id}",
    "responses": [
      {
        "bodies": [],
        "status": "204"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "cove_id"
      },
      {
        "kind": "literal",
        "value": "/folders/"
      },
      {
        "kind": "parameter",
        "name": "folder_id"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "cove_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{cove_id}/waves",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/Wave"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "cove_id"
      },
      {
        "kind": "literal",
        "value": "/waves"
      }
    ]
  },
  {
    "method": "DELETE",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{id}",
    "responses": [
      {
        "bodies": [],
        "status": "204"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "PATCH",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/coves/{id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Cove"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/coves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "path",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "query",
        "name": "old_path",
        "required": false,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/fs/gitdiff",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/GitDiffResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/fs/gitdiff"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "path",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/fs/gitstatus",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/GitStatusResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/fs/gitstatus"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "path",
        "required": false,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/fs/listdir",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ListdirResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/fs/listdir"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "path",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/fs/readfile",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ReadFileResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/fs/readfile"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "path",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/fs/readfile-raw",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/octet-stream",
            "schema": {
              "items": {
                "format": "int32",
                "minimum": 0,
                "type": "integer"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/fs/readfile-raw"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "entity_kind",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "query",
        "name": "entity_id",
        "required": false,
        "schema": {
          "type": [
            "string",
            "null"
          ]
        }
      }
    ],
    "path": "/api/overlays",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/Overlay"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/overlays"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [],
    "path": "/api/overlays",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Overlay"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/overlays"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [],
    "path": "/api/overlays/delete",
    "responses": [
      {
        "bodies": [],
        "status": "204"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/overlays/delete"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [],
    "path": "/api/plugins",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/PluginListItem"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [],
    "path": "/api/plugins/install",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/PluginDetail"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "422"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/install"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [],
    "path": "/api/plugins/views",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/ViewCatalogEntry"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/views"
      }
    ]
  },
  {
    "method": "DELETE",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}",
    "responses": [
      {
        "bodies": [],
        "status": "204"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/PluginDetail"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "PATCH",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}/config",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/PluginDetail"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/config"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}/disable",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/PluginDetail"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/disable"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}/enable",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/PluginDetail"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "422"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/enable"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "query",
        "name": "n",
        "required": false,
        "schema": {
          "minimum": 0,
          "type": [
            "integer",
            "null"
          ]
        }
      }
    ],
    "path": "/api/plugins/{id}/log",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "type": "string"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/log"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}/reload",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/PluginDetail"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "422"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/reload"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "path",
        "name": "view_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}/resources/{view_id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "text/html;profile=mcp-app",
            "schema": {
              "type": "string"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/resources/"
      },
      {
        "kind": "parameter",
        "name": "view_id"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}/rotate-token",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/PluginDetail"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/rotate-token"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/plugins/{id}/tool-call",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "type": "object"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/plugins/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/tool-call"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [],
    "path": "/api/settings",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/SettingsBag"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/settings"
      }
    ]
  },
  {
    "method": "PUT",
    "parameters": [],
    "path": "/api/settings",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/SettingsBag"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/settings"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "thread_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "query",
        "name": "provider",
        "required": false,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/threads/{thread_id}/card",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ThreadCardResolution"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/threads/"
      },
      {
        "kind": "parameter",
        "name": "thread_id"
      },
      {
        "kind": "literal",
        "value": "/card"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [],
    "path": "/api/today/launchpad/ensure",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/TodayLaunchpad"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/TodayLaunchpad"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "503"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/today/launchpad/ensure"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [],
    "path": "/api/version",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/VersionInfo"
            }
          }
        ],
        "status": "200"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/version"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "query",
        "name": "since",
        "required": false,
        "schema": {
          "format": "int64",
          "type": [
            "integer",
            "null"
          ]
        }
      },
      {
        "in": "query",
        "name": "until",
        "required": false,
        "schema": {
          "format": "int64",
          "type": [
            "integer",
            "null"
          ]
        }
      },
      {
        "in": "query",
        "name": "cove_id",
        "required": false,
        "schema": {
          "type": [
            "string",
            "null"
          ]
        }
      }
    ],
    "path": "/api/waves",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/Wave"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [],
    "path": "/api/waves",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Wave"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves"
      }
    ]
  },
  {
    "method": "DELETE",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}",
    "responses": [
      {
        "bodies": [],
        "status": "204"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/WaveDetail"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "PATCH",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Wave"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}/backlinks",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/WaveBacklinksResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/backlinks"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "query",
        "name": "path",
        "required": false,
        "schema": {
          "type": [
            "string",
            "null"
          ]
        }
      }
    ],
    "path": "/api/waves/{id}/files/cat",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/WaveFsContent"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/files/cat"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "query",
        "name": "path",
        "required": false,
        "schema": {
          "type": [
            "string",
            "null"
          ]
        }
      }
    ],
    "path": "/api/waves/{id}/files/ls",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/WaveFsEntry"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/files/ls"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}/report",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/WaveReportReadResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/report"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}/report",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/WaveReportPayload"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/report"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}/report/blocks",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ReportBlockWriteResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/report/blocks"
      }
    ]
  },
  {
    "method": "DELETE",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "path",
        "name": "block_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}/report/blocks/{block_id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ReportBlockWriteResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/report/blocks/"
      },
      {
        "kind": "parameter",
        "name": "block_id"
      }
    ]
  },
  {
    "method": "PATCH",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "path",
        "name": "block_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}/report/blocks/{block_id}",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ReportBlockWriteResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/report/blocks/"
      },
      {
        "kind": "parameter",
        "name": "block_id"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "id",
        "required": true,
        "schema": {
          "type": "string"
        }
      },
      {
        "in": "path",
        "name": "block_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{id}/report/blocks/{block_id}/move",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ReportBlockWriteResponse"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "401"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "409"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "id"
      },
      {
        "kind": "literal",
        "value": "/report/blocks/"
      },
      {
        "kind": "parameter",
        "name": "block_id"
      },
      {
        "kind": "literal",
        "value": "/move"
      }
    ]
  },
  {
    "method": "GET",
    "parameters": [
      {
        "in": "path",
        "name": "wave_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{wave_id}/cards",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "items": {
                "$ref": "#/components/schemas/Card"
              },
              "type": "array"
            }
          }
        ],
        "status": "200"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "wave_id"
      },
      {
        "kind": "literal",
        "value": "/cards"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "wave_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{wave_id}/cards",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Card"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "400"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "403"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "422"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "502"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "wave_id"
      },
      {
        "kind": "literal",
        "value": "/cards"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "wave_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{wave_id}/claude-cards",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Card"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "422"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "wave_id"
      },
      {
        "kind": "literal",
        "value": "/claude-cards"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "wave_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{wave_id}/codex-cards",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Card"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "422"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "wave_id"
      },
      {
        "kind": "literal",
        "value": "/codex-cards"
      }
    ]
  },
  {
    "method": "POST",
    "parameters": [
      {
        "in": "path",
        "name": "wave_id",
        "required": true,
        "schema": {
          "type": "string"
        }
      }
    ],
    "path": "/api/waves/{wave_id}/terminal-cards",
    "responses": [
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/Card"
            }
          }
        ],
        "status": "201"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "404"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "422"
      },
      {
        "bodies": [
          {
            "contentType": "application/json",
            "schema": {
              "$ref": "#/components/schemas/ErrorBody"
            }
          }
        ],
        "status": "500"
      }
    ],
    "template": [
      {
        "kind": "literal",
        "value": "/api/waves/"
      },
      {
        "kind": "parameter",
        "name": "wave_id"
      },
      {
        "kind": "literal",
        "value": "/terminal-cards"
      }
    ]
  }
] as const;

export const schemaWireTypes = {
  "AgentProvider": "AgentProvider",
  "BacklinkQuote": "BacklinkQuote",
  "BlockVerdict": "BlockVerdict",
  "Card": "Card",
  "CardPatch": null,
  "CardRole": "CardRole",
  "CardRuntimeView": "CardRuntimeView",
  "Cove": "Cove",
  "CoveConversationSummary": "CoveConversationSummary",
  "CoveFolder": "CoveFolder",
  "CoveKind": "CoveKind",
  "CovePatch": null,
  "CoveResolve": "CoveResolve",
  "CreateCardBody": null,
  "CreateReportBlockBody": null,
  "CreateWaveRequest": null,
  "DeleteReportBlockBody": null,
  "Diagnostic": "Diagnostic",
  "DirEntry": "DirEntry",
  "ErrorBody": null,
  "FolderConflict": "FolderConflict",
  "FolderConflictKind": "FolderConflictKind",
  "GetSpecRunResponse": null,
  "GitChangedFile": "GitChangedFile",
  "GitDiffResponse": null,
  "GitStatusResponse": null,
  "HarnessItem": "HarnessItem",
  "HarnessItemsDirection": null,
  "HarnessItemsQuery": null,
  "HarnessPhaseTag": "HarnessPhaseTag",
  "InstallBody": null,
  "InstallSource": null,
  "InterruptSpecCardResponse": null,
  "ListdirResponse": null,
  "MoveReportBlockBody": null,
  "NewCard": null,
  "NewClaudeCardBody": null,
  "NewCodexCardBody": null,
  "NewCove": null,
  "NewCoveConversationBody": null,
  "NewCoveFolder": null,
  "NewOverlay": null,
  "NewTerminalCardBody": null,
  "NewWave": null,
  "Overlay": "Overlay",
  "OverlayDeleteBody": null,
  "OverlayQuery": null,
  "Plugin": null,
  "PluginDetail": null,
  "PluginListItem": null,
  "RatifyCardDecision": "RatifyCardDecision",
  "RatifyCardRequest": null,
  "RatifyCardResponse": null,
  "ReadFileResponse": null,
  "ReportBlock": "ReportBlock",
  "ReportBlockWriteResponse": null,
  "RequestTheme": null,
  "ResetSpecCardResponse": null,
  "ResolveQuery": null,
  "SendSpecInputRequest": null,
  "SendSpecInputResponse": null,
  "SettingsBag": null,
  "SettingsPutBody": null,
  "Terminal": null,
  "ThreadCardResolution": null,
  "TodayLaunchpad": null,
  "ToolCallBody": null,
  "UpdateReportBlockBody": null,
  "UpdateWaveReportBody": null,
  "VersionInfo": null,
  "ViaToolCall": null,
  "ViewCatalogEntry": null,
  "ViewSizeWire": "ViewSizeWire",
  "Wave": "Wave",
  "WaveBacklink": "WaveBacklink",
  "WaveBacklinksResponse": null,
  "WaveDetail": null,
  "WaveFsCardMeta": "WaveFsCardMeta",
  "WaveFsCatQuery": null,
  "WaveFsContent": null,
  "WaveFsEntry": null,
  "WaveFsHookEvent": "WaveFsHookEvent",
  "WaveFsLsQuery": null,
  "WaveFsRunDetail": "WaveFsRunDetail",
  "WaveFsRunEventRef": "WaveFsRunEventRef",
  "WaveFsRunEvents": "WaveFsRunEvents",
  "WaveFsRunIndexEntry": "WaveFsRunIndexEntry",
  "WaveFsRunStatus": "WaveFsRunStatus",
  "WaveFsRunVerdict": "WaveFsRunVerdict",
  "WaveFsRunVerdictSummary": "WaveFsRunVerdictSummary",
  "WaveLifecycle": "WaveLifecycle",
  "WavePatch": null,
  "WaveReportPayload": "WaveReportPayload",
  "WaveReportReadResponse": null,
  "WavesWindowQuery": null,
  "WorkerSessionKind": "WorkerSessionKind",
  "WorkerSessionState": "WorkerSessionState"
} as const;
