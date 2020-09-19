const socket = new WebSocket("ws://localhost:9000");

socket.addEventListener('message', function(event) {
	location.reload();
});
