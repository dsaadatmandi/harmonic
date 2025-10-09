new strat:

clients maintain state of last sync
on start sync -> compare hash of current to last
anything different must be newer

stream to server:
last sync timestamp
for each file deemed modified a list of:
struct {
    hash
    path
    last modified time
}

server generates list of local path + local last modified time
anything newer than last sync time must be sent to client
if present in both lists, check latest last modified time as source of truth (more sophisticated in future)

server sends response for each path with:
struct {
    path
    Method
}
where:
enum Method {
    ServerSend
    ClientSend
}

client initiates transfer for client send, server responds with all items that are server send