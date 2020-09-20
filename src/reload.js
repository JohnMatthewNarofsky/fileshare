const address = "ws://localhost:9000";
var socket = new WebSocket(address);
function milliseconds(t) {
	return new Promise(resolve => {
		setTimeout(() => {
			resolve('resolved');
		}, t);
	});
}

function on_message(event) {
	location.reload();
}
function on_error(event) {}

async function on_close(event) {
	console.log("Reload server connection closed. Retrying...");
	socket.close();
	socket = new WebSocket(address);
	init_socket(socket);
}

function init_socket(socket) {
	socket.addEventListener('close', on_close);
	socket.addEventListener('message', on_message);
	socket.addEventListener('error', on_error);	
}

init_socket(socket);
