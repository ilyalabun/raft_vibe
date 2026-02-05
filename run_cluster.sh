#!/bin/bash
# Run a 3-node Raft cluster locally
#
# Usage: ./run_cluster.sh [start|stop|clean|status|killnode <id>|startnode <id>|watchlogs]

set -e

DATA_DIR="/tmp/raft-cluster"
LOG_DIR="$DATA_DIR/logs"

start_cluster() {
    echo "Building raft-server..."
    cargo build --release --bin raft-server

    echo "Creating data directories..."
    mkdir -p "$DATA_DIR/node1" "$DATA_DIR/node2" "$DATA_DIR/node3" "$LOG_DIR"

    echo "Starting 3-node Raft cluster..."
    echo ""

    # Start node 1
    echo "Starting node 1 (transport=8001, api=9001)..."
    ./target/release/raft-server \
        --id 1 \
        --transport-port 8001 \
        --api-port 9001 \
        --data-dir "$DATA_DIR/node1" \
        --peers 2=127.0.0.1:8002,3=127.0.0.1:8003 \
        --snapshot-threshold 10 \
        > "$LOG_DIR/node1.log" 2>&1 &
    echo $! > "$DATA_DIR/node1.pid"

    # Start node 2
    echo "Starting node 2 (transport=8002, api=9002)..."
    ./target/release/raft-server \
        --id 2 \
        --transport-port 8002 \
        --api-port 9002 \
        --data-dir "$DATA_DIR/node2" \
        --peers 1=127.0.0.1:8001,3=127.0.0.1:8003 \
        --snapshot-threshold 10 \
        > "$LOG_DIR/node2.log" 2>&1 &
    echo $! > "$DATA_DIR/node2.pid"

    # Start node 3
    echo "Starting node 3 (transport=8003, api=9003)..."
    ./target/release/raft-server \
        --id 3 \
        --transport-port 8003 \
        --api-port 9003 \
        --data-dir "$DATA_DIR/node3" \
        --peers 1=127.0.0.1:8001,2=127.0.0.1:8002 \
        --snapshot-threshold 10 \
        > "$LOG_DIR/node3.log" 2>&1 &
    echo $! > "$DATA_DIR/node3.pid"

    echo ""
    echo "Cluster started!"
    echo ""
    echo "Configuration:"
    echo "  Snapshot threshold: 10 entries (snapshots trigger frequently for testing)"
    echo ""
    echo "Node endpoints:"
    echo "  Node 1: transport=http://127.0.0.1:8001, api=http://127.0.0.1:9001"
    echo "  Node 2: transport=http://127.0.0.1:8002, api=http://127.0.0.1:9002"
    echo "  Node 3: transport=http://127.0.0.1:8003, api=http://127.0.0.1:9003"
    echo ""
    echo "Client API examples (use API port 900X):"
    echo "  # Check status"
    echo "  curl http://127.0.0.1:9001/client/status"
    echo ""
    echo "  # Check leader"
    echo "  curl http://127.0.0.1:9001/client/leader"
    echo ""
    echo "  # Submit a command (to leader)"
    echo "  curl -X POST http://127.0.0.1:9001/client/submit \\"
    echo "       -H 'Content-Type: application/json' \\"
    echo "       -d '{\"command\": \"SET mykey myvalue\"}'"
    echo ""
    echo "  # Submit many commands to see automatic snapshots:"
    echo "  for i in {1..15}; do curl -X POST http://127.0.0.1:9001/client/submit \\"
    echo "       -H 'Content-Type: application/json' \\"
    echo "       -d \"{\\\"command\\\": \\\"SET key\$i value\$i\\\"}\"; done"
    echo ""
    echo "Logs: $LOG_DIR/"
    echo "  Watch for: '[NODE X] Automatic snapshot triggered'"
    echo "To stop: ./run_cluster.sh stop"
}

stop_cluster() {
    echo "Stopping cluster..."
    for i in 1 2 3; do
        if [ -f "$DATA_DIR/node$i.pid" ]; then
            pid=$(cat "$DATA_DIR/node$i.pid")
            if kill -0 "$pid" 2>/dev/null; then
                echo "Stopping node $i (PID $pid)..."
                kill "$pid" 2>/dev/null || true
            fi
            rm -f "$DATA_DIR/node$i.pid"
        fi
    done
    echo "Cluster stopped."
}

clean_cluster() {
    stop_cluster
    echo "Cleaning data directories..."
    rm -rf "$DATA_DIR"
    echo "Clean complete."
}

status_cluster() {
    echo "Cluster status:"
    for i in 1 2 3; do
        api_port=$((9000 + i))
        echo -n "  Node $i (api port $api_port): "
        if curl -s "http://127.0.0.1:$api_port/client/status" > /dev/null 2>&1; then
            state=$(curl -s "http://127.0.0.1:$api_port/client/status" | grep -o '"state":"[^"]*"' | cut -d'"' -f4)
            term=$(curl -s "http://127.0.0.1:$api_port/client/status" | grep -o '"term":[0-9]*' | cut -d':' -f2)
            echo "$state (term $term)"
        else
            echo "not responding"
        fi
    done
}

kill_node() {
    local node_id=$1
    if [ -z "$node_id" ] || [ "$node_id" -lt 1 ] || [ "$node_id" -gt 3 ]; then
        echo "Usage: $0 killnode <1|2|3>"
        exit 1
    fi

    if [ -f "$DATA_DIR/node$node_id.pid" ]; then
        pid=$(cat "$DATA_DIR/node$node_id.pid")
        if kill -0 "$pid" 2>/dev/null; then
            echo "Killing node $node_id (PID $pid)..."
            kill "$pid" 2>/dev/null || true
            echo "Node $node_id killed."
        else
            echo "Node $node_id is not running."
        fi
    else
        echo "Node $node_id PID file not found. Is the cluster running?"
    fi
}

start_node() {
    local node_id=$1
    if [ -z "$node_id" ] || [ "$node_id" -lt 1 ] || [ "$node_id" -gt 3 ]; then
        echo "Usage: $0 startnode <1|2|3>"
        exit 1
    fi

    # Check if already running
    if [ -f "$DATA_DIR/node$node_id.pid" ]; then
        pid=$(cat "$DATA_DIR/node$node_id.pid")
        if kill -0 "$pid" 2>/dev/null; then
            echo "Node $node_id is already running (PID $pid)."
            return
        fi
    fi

    # Build peers list (all nodes except this one)
    local peers=""
    for i in 1 2 3; do
        if [ "$i" -ne "$node_id" ]; then
            if [ -n "$peers" ]; then
                peers="$peers,"
            fi
            peers="$peers$i=127.0.0.1:$((8000 + i))"
        fi
    done

    local transport_port=$((8000 + node_id))
    local api_port=$((9000 + node_id))
    echo "Starting node $node_id (transport=$transport_port, api=$api_port)..."
    ./target/release/raft-server \
        --id "$node_id" \
        --transport-port "$transport_port" \
        --api-port "$api_port" \
        --data-dir "$DATA_DIR/node$node_id" \
        --peers "$peers" \
        --snapshot-threshold 10 \
        > "$LOG_DIR/node$node_id.log" 2>&1 &
    echo $! > "$DATA_DIR/node$node_id.pid"
    echo "Node $node_id started (PID $!)."
}

watch_logs() {
    echo "Watching cluster logs (Ctrl+C to stop)..."
    echo ""
    # Colors: Node 1 = Blue, Node 2 = Green, Node 3 = Yellow
    tail -f "$LOG_DIR/node1.log" "$LOG_DIR/node2.log" "$LOG_DIR/node3.log" 2>/dev/null | \
    grep --line-buffered -v -e "^==> " -e "^$" | \
    sed -u \
        -e "s/\[NODE 1\]/\x1b[34m[NODE 1]\x1b[0m/" \
        -e "s/\[NODE 2\]/\x1b[32m[NODE 2]\x1b[0m/" \
        -e "s/\[NODE 3\]/\x1b[33m[NODE 3]\x1b[0m/"
}

case "${1:-start}" in
    start)
        start_cluster
        ;;
    stop)
        stop_cluster
        ;;
    clean)
        clean_cluster
        ;;
    status)
        status_cluster
        ;;
    killnode)
        kill_node "$2"
        ;;
    startnode)
        start_node "$2"
        ;;
    watchlogs)
        watch_logs
        ;;
    *)
        echo "Usage: $0 [start|stop|clean|status|killnode <id>|startnode <id>|watchlogs]"
        exit 1
        ;;
esac
