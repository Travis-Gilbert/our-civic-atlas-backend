from civic_atlas.v1 import civic_atlas_pb2
from civic_atlas.v1 import civic_atlas_pb2_grpc
from civic_atlas.v1 import reconstruction_pb2
from civic_atlas.v1 import reconstruction_pb2_grpc
from civic_atlas.v1 import reconstruction_service_pb2
from civic_atlas.v1 import reconstruction_service_pb2_grpc
from civic_atlas.v1 import spacetime_atlas_pb2
from civic_atlas.v1 import spacetime_atlas_pb2_grpc
from theseus_bridge.v1 import bridge_pb2
from theseus_bridge.v1 import bridge_pb2_grpc


def main() -> None:
    modules = [
        civic_atlas_pb2,
        civic_atlas_pb2_grpc,
        reconstruction_pb2,
        reconstruction_pb2_grpc,
        reconstruction_service_pb2,
        reconstruction_service_pb2_grpc,
        spacetime_atlas_pb2,
        spacetime_atlas_pb2_grpc,
        bridge_pb2,
        bridge_pb2_grpc,
    ]
    missing = [module.__name__ for module in modules if not module.__name__]
    if missing:
        raise RuntimeError(f"generated Python modules failed import smoke: {missing}")
    print("Generated Python proto import gate passed.")


if __name__ == "__main__":
    main()
