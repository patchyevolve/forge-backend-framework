import os
import grpc
import asyncio
import forge_plugin_v1_pb2 as pb
import forge_plugin_v1_pb2_grpc as pb_grpc


class EchoPlugin(pb_grpc.ForgePluginServicer):
    async def Register(self, request, context):
        return pb.RegisterResponse(
            plugin_protocol_version="1.0",
            capabilities=[
                pb.Capability(
                    name="forge.example.echo",
                    version="1.0.0",
                    input_schema_ref="raw text",
                    output_schema_ref="raw text",
                )
            ],
        )

    async def Invoke(self, request, context):
        if request.capability == "forge.example.echo":
            text = request.payload.decode("utf-8")
            return pb.InvokeResponse(
                request_id=request.request_id,
                payload=text.upper().encode("utf-8"),
            )
        return pb.InvokeResponse(
            request_id=request.request_id,
            error=pb.PluginError(code="NOT_FOUND", message=f"unknown capability: {request.capability}"),
        )

    async def HealthCheck(self, request, context):
        return pb.HealthCheckResponse(healthy=True, detail="ok")

    async def Drain(self, request, context):
        return pb.DrainResponse()


async def main():
    callback_addr = os.environ["FORGE_CALLBACK_ADDR"]
    # the kernel hands us http://host:port but grpc.aio just wants host:port
    if callback_addr.startswith("http://"):
        callback_addr = callback_addr[len("http://"):]
    elif callback_addr.startswith("https://"):
        callback_addr = callback_addr[len("https://"):]
    server = grpc.aio.server()
    pb_grpc.add_ForgePluginServicer_to_server(EchoPlugin(), server)
    server.add_insecure_port(callback_addr)
    await server.start()
    await server.wait_for_termination()


if __name__ == "__main__":
    asyncio.run(main())
